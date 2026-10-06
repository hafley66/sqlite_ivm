//! Several programs as one install. The parts share nothing but the dictionary: a constructor
//! relation is one relation per name, every other relation belongs to one part.

use crate::{Expr, LetRec, Op, Program, RelId, RelKind, Relation, Stratum, TermArg, TermLit, Ty};
use std::collections::BTreeMap;

pub struct Composed {
    pub program: Program,
    /// Per part: the part's relation id to the composed program's relation id.
    pub rels: Vec<BTreeMap<RelId, RelId>>,
}

/// `expr` with every `Text` index shifted by `base` and every `Term` index by `terms`: post-order
/// over an explicit stack.
fn texts(expr: &Expr, base: u32, terms: u32) -> Expr {
    enum Step<'e> { Enter(&'e Expr), Build(crate::Func, usize) }
    let mut steps = vec![Step::Enter(expr)];
    let mut done: Vec<Expr> = Vec::new();
    while let Some(step) = steps.pop() {
        match step {
            Step::Enter(Expr::Text(index)) => done.push(Expr::Text(index + base)),
            Step::Enter(Expr::Term(index)) => done.push(Expr::Term(index + terms)),
            Step::Enter(Expr::Call(func, args)) => {
                steps.push(Step::Build(*func, args.len()));
                steps.extend(args.iter().rev().map(Step::Enter));
            }
            Step::Enter(other) => done.push(other.clone()),
            Step::Build(func, arity) => {
                let args = done.split_off(done.len() - arity);
                done.push(Expr::Call(func, args));
            }
        }
    }
    done.pop().expect("one expression")
}

/// One LetRec level; `LetRec::nested` is one level deep.
fn letrec_level(rec: &LetRec, rel: &impl Fn(RelId) -> RelId, node: u32) -> LetRec {
    LetRec {
        ids: rec.ids.iter().map(|id| rel(*id)).collect(),
        bodies: rec.bodies.iter().map(|body| body + node).collect(),
        limit: rec.limit,
        nested: Vec::new(),
    }
}

fn letrec(rec: &LetRec, rel: &impl Fn(RelId) -> RelId, node: u32) -> LetRec {
    LetRec {
        nested: rec.nested.iter().map(|inner| letrec_level(inner, rel, node)).collect(),
        ..letrec_level(rec, rel, node)
    }
}

/// `parts` as `(prefix, program)`, concatenated in order. `Err`
/// names a constructor two parts declare with different columns.
pub fn compose(parts: &[(&str, &Program)]) -> Result<Composed, String> {
    let mut out = Program { texts: Vec::new(), terms: Vec::new(), rels: Vec::new(), nodes: Vec::new(), strata: Vec::new(), outputs: Vec::new() };
    let mut constructors: BTreeMap<String, (RelId, Vec<Ty>)> = BTreeMap::new();
    let mut maps = Vec::with_capacity(parts.len());
    let mut next: RelId = 0;
    for (prefix, part) in parts {
        let mut map = BTreeMap::new();
        for rel in &part.rels {
            let id = if rel.kind == RelKind::Constructor {
                match constructors.get(&rel.name) {
                    Some((id, cols)) if *cols == rel.cols => *id,
                    Some(_) => return Err(format!("constructor {} declared with two column lists", rel.name)),
                    None => {
                        let id = next;
                        next += 1;
                        constructors.insert(rel.name.clone(), (id, rel.cols.clone()));
                        out.rels.push(Relation { id, name: rel.name.clone(), cols: rel.cols.clone(), kind: RelKind::Constructor });
                        id
                    }
                }
            } else {
                let id = next;
                next += 1;
                out.rels.push(Relation { id, name: format!("{prefix}{}", rel.name), cols: rel.cols.clone(), kind: rel.kind.clone() });
                id
            };
            map.insert(rel.id, id);
        }
        let node_base = out.nodes.len() as u32;
        let text_base = out.texts.len() as u32;
        let term_base = out.terms.len() as u32;
        let rel = |id: RelId| map[&id];
        for op in &part.nodes {
            let n = |id: u32| id + node_base;
            out.nodes.push(match op {
                Op::Get(id) => Op::Get(rel(*id)),
                Op::Mint { input, functor, args } => Op::Mint { input: n(*input), functor: rel(*functor), args: args.clone() },
                Op::StrCons { input, mode } => Op::StrCons { input: n(*input), mode: mode.clone() },
                Op::Str { input, op, args } => Op::Str { input: n(*input), op: *op, args: args.clone() },
                Op::Mfp { input, filter, map, project } => Op::Mfp {
                    input: n(*input),
                    filter: filter.iter().map(|e| texts(e, text_base, term_base)).collect(),
                    map: map.iter().map(|e| texts(e, text_base, term_base)).collect(),
                    project: project.clone(),
                },
                Op::Union(inputs) => Op::Union(inputs.iter().map(|i| n(*i)).collect()),
                Op::Negate(input) => Op::Negate(n(*input)),
                Op::Join { inputs, equivalences, project } => Op::Join { inputs: inputs.iter().map(|i| n(*i)).collect(), equivalences: equivalences.clone(), project: project.clone() },
                Op::Antijoin { l, r, lk, rk } => Op::Antijoin { l: n(*l), r: n(*r), lk: lk.clone(), rk: rk.clone() },
                Op::Reduce { input, key, aggs } => Op::Reduce { input: n(*input), key: key.clone(), aggs: aggs.clone() },
                Op::Threshold(input) => Op::Threshold(n(*input)),
                Op::TopK { input, key, order, limit } => Op::TopK { input: n(*input), key: key.clone(), order: order.clone(), limit: *limit },
                Op::Window { input, partition, order, func } => Op::Window { input: n(*input), partition: partition.clone(), order: order.clone(), func: func.clone() },
                Op::Delay(input) => Op::Delay(n(*input)),
            });
        }
        for stratum in &part.strata {
            out.strata.push(match stratum {
                Stratum::Let { id, body } => Stratum::Let { id: rel(*id), body: body + node_base },
                Stratum::LetRec(rec) => Stratum::LetRec(letrec(rec, &rel, node_base)),
            });
        }
        out.outputs.extend(part.outputs.iter().map(|id| rel(*id)));
        out.texts.extend(part.texts.iter().cloned());
        out.terms.extend(part.terms.iter().map(|term| TermLit {
            functor: rel(term.functor),
            args: term.args.iter().map(|arg| match arg {
                TermArg::Term(index) => TermArg::Term(index + term_base),
                TermArg::Text(index) => TermArg::Text(index + text_base),
                TermArg::Raw(cell) => TermArg::Raw(*cell),
            }).collect(),
        }));
        maps.push(map);
    }
    Ok(Composed { program: out, rels: maps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str) -> Program {
        Program {
            texts: vec![format!("{name} text")],
            terms: vec![
                TermLit { functor: 1, args: vec![TermArg::Text(0)] },
                TermLit { functor: 1, args: vec![TermArg::Term(0)] },
            ],
            rels: vec![
                Relation { id: 0, name: "src".into(), cols: vec![Ty::Int], kind: RelKind::Source },
                Relation { id: 1, name: "9:1:f".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Constructor },
                Relation { id: 2, name: "out".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
            ],
            nodes: vec![Op::Get(0), Op::Mfp { input: 0, filter: vec![Expr::Call(crate::Func::Ne, vec![Expr::Col(0), Expr::Text(0)])], map: vec![Expr::Term(1)], project: vec![] }],
            strata: vec![Stratum::Let { id: 2, body: 1 }],
            outputs: vec![2],
        }
    }

    #[test]
    fn parts_share_constructors_and_keep_their_relations() {
        let (a, b) = (part("a"), part("b"));
        let composed = compose(&[("a.", &a), ("b.", &b)]).unwrap();
        let names: Vec<(RelId, &str)> = composed.program.rels.iter().map(|r| (r.id, r.name.as_str())).collect();
        assert_eq!(names, vec![(0, "a.src"), (1, "9:1:f"), (2, "a.out"), (3, "b.src"), (4, "b.out")]);
        assert_eq!(composed.rels[1].get(&1), Some(&1));
        assert_eq!(composed.program.nodes[3], Op::Mfp { input: 2, filter: vec![Expr::Call(crate::Func::Ne, vec![Expr::Col(0), Expr::Text(1)])], map: vec![Expr::Term(3)], project: vec![] });
        assert_eq!(composed.program.outputs, vec![2, 4]);
        assert_eq!(composed.program.texts, vec!["a text".to_string(), "b text".to_string()]);
        assert_eq!(composed.program.terms[2..], [
            TermLit { functor: 1, args: vec![TermArg::Text(1)] },
            TermLit { functor: 1, args: vec![TermArg::Term(2)] },
        ]);
    }
}
