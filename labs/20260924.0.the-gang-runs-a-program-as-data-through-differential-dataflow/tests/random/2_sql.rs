//! IR to SQLite DDL with bag semantics, written from the IR docs alone; shares no code with any engine.
//! Every relation exposes columns c0..cN; every node prints as one SELECT over aliased subqueries.

use lab_20260924_0::*;
use std::fmt::Write as _;

pub fn arity(p: &Program, n: NodeId) -> usize {
    match &p.nodes[n as usize] {
        Op::Get(rel) => p.rel(*rel).unwrap().cols.len(),
        Op::Mfp { input, map, project, .. } => {
            if project.is_empty() {
                arity(p, *input) + map.len()
            } else {
                project.len()
            }
        }
        Op::Union(ns) => arity(p, ns[0]),
        Op::Negate(n) | Op::Threshold(n) => arity(p, *n),
        Op::Join { inputs, .. } => inputs.iter().map(|n| arity(p, *n)).sum(),
        Op::Antijoin { l, .. } => arity(p, *l),
        Op::Reduce { key, aggs, .. } => key.len() + aggs.len(),
        op => panic!("printer: unsupported {op:?}"),
    }
}

fn names(n: usize) -> String {
    (0..n).map(|i| format!("c{i}")).collect::<Vec<_>>().join(", ")
}

/// Source tables (all-column primary key, since sources are sets), then one view per stratum.
pub fn ddl(p: &Program) -> String {
    let mut out = String::new();
    for rel in p.rels.iter().filter(|r| r.kind == RelKind::Source) {
        let n = rel.cols.len();
        let defs: Vec<String> = (0..n).map(|i| format!("c{i} INTEGER NOT NULL")).collect();
        writeln!(out, "CREATE TABLE \"{}\"({}, PRIMARY KEY({}));", rel.name, defs.join(", "), names(n)).unwrap();
    }
    let mut printer = Printer { p, next: 0 };
    for stratum in &p.strata {
        let Stratum::Let { id, body } = stratum else { panic!("printer: LetRec") };
        let rel = p.rel(*id).unwrap();
        let body = printer.node(*body);
        writeln!(out, "CREATE VIEW \"{}\"({}) AS {body};", rel.name, names(rel.cols.len())).unwrap();
    }
    out
}

struct Printer<'p> {
    p: &'p Program,
    next: usize,
}

impl Printer<'_> {
    fn alias(&mut self) -> String {
        self.next += 1;
        format!("t{}", self.next)
    }

    fn node(&mut self, n: NodeId) -> String {
        let p = self.p;
        match &p.nodes[n as usize] {
            Op::Get(rel) => {
                let rel = p.rel(*rel).unwrap();
                format!("SELECT {} FROM \"{}\"", names(rel.cols.len()), rel.name)
            }
            Op::Mfp { input, filter, map, project } => {
                let (t, a, inner) = (self.alias(), arity(p, *input), self.node(*input));
                let out: Vec<usize> =
                    if project.is_empty() { (0..a + map.len()).collect() } else { project.iter().map(|c| *c as usize).collect() };
                let sel: Vec<String> = out.iter().enumerate().map(|(i, c)| format!("{} AS c{i}", col(&t, a, map, *c))).collect();
                let mut s = format!("SELECT {} FROM ({inner}) AS {t}", sel.join(", "));
                if !filter.is_empty() {
                    let wheres: Vec<String> = filter.iter().map(|e| format!("{} <> 0", expr(&t, a, map, e))).collect();
                    write!(s, " WHERE {}", wheres.join(" AND ")).unwrap();
                }
                s
            }
            Op::Union(ns) => {
                let parts: Vec<String> = ns.iter().map(|n| format!("SELECT * FROM ({})", self.node(*n))).collect();
                parts.join(" UNION ALL ")
            }
            Op::Join { inputs, equivalences } => {
                let mut sel = Vec::new();
                let mut from = Vec::new();
                let mut ts = Vec::new();
                for input in inputs {
                    let (t, inner) = (self.alias(), self.node(*input));
                    for c in 0..arity(p, *input) {
                        sel.push(format!("{t}.c{c} AS c{}", sel.len()));
                    }
                    from.push(format!("({inner}) AS {t}"));
                    ts.push(t);
                }
                let mut on = Vec::new();
                for class in equivalences {
                    let (i0, c0) = class[0];
                    for (i, c) in &class[1..] {
                        on.push(format!("{}.c{c0} = {}.c{c}", ts[i0 as usize], ts[*i as usize]));
                    }
                }
                let on = if on.is_empty() { "1".to_string() } else { on.join(" AND ") };
                format!("SELECT {} FROM {} ON {on}", sel.join(", "), from.join(" JOIN "))
            }
            Op::Antijoin { l, r, lk, rk } => {
                let (tl, ls) = (self.alias(), self.node(*l));
                let (tr, rs) = (self.alias(), self.node(*r));
                let sel: Vec<String> = (0..arity(p, *l)).map(|c| format!("{tl}.c{c} AS c{c}")).collect();
                let on: Vec<String> = lk.iter().zip(rk).map(|(a, b)| format!("{tr}.c{b} = {tl}.c{a}")).collect();
                let on = if on.is_empty() { String::new() } else { format!(" WHERE {}", on.join(" AND ")) };
                format!("SELECT {} FROM ({ls}) AS {tl} WHERE NOT EXISTS (SELECT 1 FROM ({rs}) AS {tr}{on})", sel.join(", "))
            }
            Op::Reduce { input, key, aggs } => {
                let (t, inner) = (self.alias(), self.node(*input));
                let keys: Vec<String> = key.iter().map(|k| format!("{t}.c{k}")).collect();
                let mut sel = keys.clone();
                sel.extend(aggs.iter().map(|agg| match agg {
                    Agg::Count => "count(*)".to_string(),
                    Agg::Sum(c) => format!("sum({t}.c{c})"),
                    Agg::Min(c) => format!("min({t}.c{c})"),
                    Agg::Max(c) => format!("max({t}.c{c})"),
                }));
                let sel: Vec<String> = sel.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect();
                let tail = if keys.is_empty() { " HAVING count(*) > 0".to_string() } else { format!(" GROUP BY {}", keys.join(", ")) };
                format!("SELECT {} FROM ({inner}) AS {t}{tail}", sel.join(", "))
            }
            Op::Threshold(n) => format!("SELECT DISTINCT * FROM ({})", self.node(*n)),
            op => panic!("printer: unsupported {op:?}"),
        }
    }
}

/// Column `c` of an Mfp row: an input column, or a map column inlined as its expression.
fn col(t: &str, a: usize, map: &[Expr], c: usize) -> String {
    if c < a {
        format!("{t}.c{c}")
    } else {
        expr(t, a, map, &map[c - a])
    }
}

/// Every call renders to an integer; comparisons and logic yield 0 or 1.
fn expr(t: &str, a: usize, map: &[Expr], e: &Expr) -> String {
    match e {
        Expr::Col(c) => col(t, a, map, *c as usize),
        Expr::Lit(v) => format!("({v})"),
        Expr::Call(f, args) => {
            let x = |i: usize| expr(t, a, map, &args[i]);
            let op = match f {
                Func::Eq => "=",
                Func::Ne => "<>",
                Func::Lt => "<",
                Func::Le => "<=",
                Func::Gt => ">",
                Func::Ge => ">=",
                Func::Add => "+",
                Func::Sub => "-",
                Func::And => return format!("({} <> 0 AND {} <> 0)", x(0), x(1)),
                Func::Or => return format!("({} <> 0 OR {} <> 0)", x(0), x(1)),
                Func::Not => return format!("({} = 0)", x(0)),
            };
            format!("({} {op} {})", x(0), x(1))
        }
    }
}
