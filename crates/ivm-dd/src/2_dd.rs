//! differential-dataflow engine: `lower` runs once inside `worker.dataflow`; each `settle` is one epoch.

use ivm_ir::*;
use ivm_engine::*;
use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::arrange::{Arranged, TraceAgent};
use differential_dataflow::trace::implementations::ValSpine;
use differential_dataflow::lattice::Lattice;
use differential_dataflow::operators::iterate::Variable;
use differential_dataflow::{AsCollection, VecCollection};
use std::hash::{DefaultHasher, Hash, Hasher};
use timely::dataflow::operators::vec::Partition;
use timely::dataflow::Scope;
use timely::order::Product;
use timely::progress::Timestamp;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use std::time::Duration;

type Time = u64;
type Coll<'s, T = Time> = VecCollection<'s, T, Row, W>;
type Inner = Product<Time, u64>;
/// A collection keyed on some of its columns: `(key, row)`.
type Keyed<'s, T> = Arranged<'s, TraceAgent<ValSpine<Row, Row, T, W>>>;
/// Rows supplied by an arrangement to a hierarchical reduce closure.
pub type ReduceReadEventBuilder = timely::container::CapacityContainerBuilder<Vec<(Duration, usize)>>;
type ReduceReadLogger = timely::logging_core::Logger<ReduceReadEventBuilder>;

fn rec_inputs(p: &Program, rec: &LetRec) -> BTreeSet<RelId> {
    let mut inputs = BTreeSet::new();
    let mut visited = vec![false; p.nodes.len()];
    let mut pending = rec.bodies.clone();
    while let Some(id) = pending.pop() {
        let index = id as usize;
        let Some(seen) = visited.get_mut(index) else { continue; };
        if *seen { continue; }
        *seen = true;
        match &p.nodes[index] {
            Op::Get(rel) => { inputs.insert(*rel); }
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Reduce { input, .. } | Op::TopK { input, .. } | Op::Window { input, .. } => pending.push(*input),
            Op::Negate(input) | Op::Threshold(input) | Op::Delay(input) => pending.push(*input),
            Op::Union(nodes) | Op::Join { inputs: nodes, .. } => pending.extend(nodes.iter().copied()),
            Op::Antijoin { l, r, .. } => pending.extend([*l, *r]),
        }
    }
    inputs
}

/// The nodes a LetRec's loop reads that depend on none of its relations: every body that reads
/// no recursive relation, and every such input of a node that does. Each is the root of a
/// subtree the loop can read as one entered collection.
fn independent_inputs<C>(p: &Program, rec: &LetRec, defined: &[(RelId, C)], fuse: bool) -> BTreeSet<NodeId> {
    // Post-order over the nodes the bodies reach, inputs first, with an explicit stack.
    let mut dependent: Vec<Option<bool>> = vec![None; p.nodes.len()];
    let mut stack: Vec<(NodeId, bool)> = rec.bodies.iter().map(|body| (*body, false)).collect();
    while let Some((id, expanded)) = stack.pop() {
        if dependent[id as usize].is_some() { continue; }
        let op = &p.nodes[id as usize];
        let inputs = lowered_inputs(p, defined, op, fuse);
        if expanded {
            let reads_rec = matches!(op, Op::Get(rel) if rec.ids.contains(rel));
            let value = reads_rec || inputs.iter().any(|input| dependent[*input as usize] == Some(true));
            dependent[id as usize] = Some(value);
            continue;
        }
        stack.push((id, true));
        stack.extend(inputs.iter().filter(|input| dependent[**input as usize].is_none()).map(|input| (*input, false)));
    }
    let mut roots: BTreeSet<NodeId> = rec.bodies.iter().copied().filter(|body| dependent[*body as usize] == Some(false)).collect();
    for (id, value) in dependent.iter().enumerate() {
        if *value == Some(true) {
            roots.extend(lowered_inputs(p, defined, &p.nodes[id], fuse).into_iter().filter(|input| dependent[*input as usize] == Some(false)));
        }
    }
    roots
}

/// Timestamps the algebra runs at. Only the top level may open a LetRec scope; this bounds monomorphization.
pub trait Nest: Timestamp + Lattice + Ord + Hash + Clone + std::fmt::Debug + 'static {
    /// `(outer tick, loop round)`; the round is `None` outside a LetRec.
    fn split(&self) -> (Time, Option<u64>);
    fn letrec<'s>(rel: &mut DdRel<'s, Self>, p: &Program, rec: &LetRec, defined: &[(RelId, Coll<'s, Self>)])
        -> Result<Vec<Coll<'s, Self>>, EngineError>;
}

impl Nest for Inner {
    fn split(&self) -> (Time, Option<u64>) {
        (self.outer, Some(self.inner))
    }
    fn letrec<'s>(_: &mut DdRel<'s, Self>, _: &Program, _: &LetRec, _: &[(RelId, Coll<'s, Self>)])
        -> Result<Vec<Coll<'s, Self>>, EngineError> {
        Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("LetRec nested in LetRec")))
    }
}

impl Nest for Time {
    fn split(&self) -> (Time, Option<u64>) {
        (*self, None)
    }
    fn letrec<'s>(rel: &mut DdRel<'s, Self>, p: &Program, rec: &LetRec, defined: &[(RelId, Coll<'s>)])
        -> Result<Vec<Coll<'s>>, EngineError> {
        let used = rec_inputs(p, rec);
        if rec.limit.is_some() {
            return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("LetRec limit")));
        }
        if !rec.ids.iter().any(|id| used.contains(id)) {
            let mut nodes = vec![None; p.nodes.len()];
            return rec.bodies.iter().map(|body| {
                let c = lower_node(p, rel, &mut nodes, defined, *body)?;
                Ok(rel.threshold(c))
            }).collect();
        }
        // Nodes of the bodies that read no recursive relation are lowered once in the outer scope
        // and entered; the loop then iterates only over the nodes that depend on its variables.
        let hoisted: Vec<(NodeId, Coll<'s>)> = {
            let mut outer_nodes = vec![None; p.nodes.len()];
            independent_inputs(p, rec, defined, rel.fuses_join_project())
                .into_iter()
                .map(|id| Ok((id, lower_node(p, rel, &mut outer_nodes, defined, id)?)))
                .collect::<Result<_, EngineError>>()?
        };
        let outer = rel.scope;
        outer.scoped::<Inner, _, _>("LetRec", |sub| {
            let mut inner = DdRel {
                scope: sub,
                rec: rec.ids.first().copied(),
                sources: BTreeMap::new(),
                outputs: Vec::new(),
                taps: rel.taps.clone(),
                reduce_reads: rel.reduce_reads.clone(),
                constructors: rel.constructors.clone(),
                interner: rel.interner.clone(),
                sum_error: rel.sum_error.clone(),
                texts: rel.texts.clone(),
                keyed: BTreeMap::new(),
                read: BTreeSet::new(),
                types: rel.types.clone(),
                arranged: rel.arranged.clone(),
            };
            let mut nodes = vec![None; p.nodes.len()];
            for (id, c) in hoisted {
                nodes[id as usize] = Some(c.enter(sub));
            }
            let mut scope_defined: Vec<(RelId, Coll<'_, Inner>)> = Vec::new();
            let mut variables = Vec::new();
            for id in &rec.ids {
                let (variable, current) = Variable::new(sub, Product::new(Default::default(), 1));
                variables.push(variable);
                scope_defined.push((*id, current));
            }
            let mut results = Vec::new();
            for body in &rec.bodies {
                let c = lower_node(p, &mut inner, &mut nodes, &scope_defined, *body)?;
                results.push(inner.threshold(c));
            }
            for (variable, result) in variables.into_iter().zip(&results) {
                variable.set(result.clone());
            }
            if results.len() == 1 {
                return Ok(results.into_iter().map(|result| result.leave(outer)).collect());
            }
            // One scope output for the whole fixpoint: timely summarizes reachability from every
            // location in the scope to every scope output, so each extra output multiplies the
            // install and progress-tracking cost of the scope.
            let parts = results.len() as u64;
            let tagged = results.into_iter().enumerate().map(|(index, result)| {
                result.map(move |mut row| {
                    row.push(index as Cell);
                    row
                })
            });
            let left = differential_dataflow::collection::concatenate(sub, tagged).leave(outer);
            Ok(left
                .inner
                .partition(parts, |(mut row, time, w): (Row, Time, W)| {
                    let index = row.pop().expect("tagged row") as u64;
                    (index, (row, time, w))
                })
                .into_iter()
                .map(|stream| stream.as_collection())
                .collect())
        })
    }
}

pub struct DdRel<'s, T: Nest = Time> {
    scope: Scope<'s, T>,
    rec: Option<RelId>,
    sources: BTreeMap<RelId, Coll<'s, T>>,
    outputs: Vec<(RelId, Coll<'s, T>)>,
    /// Traced mode only: every observed node's records land here.
    taps: Option<Rc<RefCell<Vec<DdTap>>>>,
    reduce_reads: Option<ReduceReadLogger>,
    constructors: BTreeMap<RelId, (String, Vec<Ty>)>,
    interner: Rc<RefCell<Interner>>,
    sum_error: Rc<RefCell<Option<String>>>,
    texts: Vec<Cell>,
    /// Join inputs arranged once per `(stream, key columns)`; every join reading the same
    /// stream on the same key shares the arrangement.
    keyed: BTreeMap<(usize, usize, Vec<ColId>), Keyed<'s, T>>,
    /// Relations `get` handed a collection for.
    read: BTreeSet<RelId>,
    /// `Program::node_types_memo` answers, shared by every scope of one install.
    types: Rc<RefCell<Vec<Option<Option<Vec<Ty>>>>>>,
    /// Join arrangements built and cells sent into them, shared by every scope of one install.
    arranged: Rc<RefCell<ArrangeStats>>,
}

/// `dd install` / `dd settle` debug fields: join arrangements built at install, and cells (key
/// plus row columns, one per record update) sent into them since install.
#[derive(Default)]
struct ArrangeStats {
    arrangements: u64,
    cells: u64,
}

impl<'s, T: Nest> DdRel<'s, T> {
    fn keyed(&mut self, c: Coll<'s, T>, key: Vec<ColId>, any: Vec<bool>) -> Keyed<'s, T> {
        let source = *c.inner.name();
        let id = (source.node, source.port, key.clone());
        if let Some(arranged) = self.keyed.get(&id) {
            return arranged.clone();
        }
        let interner = self.interner.clone();
        self.arranged.borrow_mut().arrangements += 1;
        let stats = self.arranged.clone();
        let arranged = c
            .flat_map(move |row| {
                stats.borrow_mut().cells += (key.len() + row.len()) as u64;
                let mut cells = cols(&row, &key);
                for (cell, any) in cells.iter_mut().zip(&any) {
                    if *any {
                        let dict = interner.borrow();
                        if matches!(dict.any_value(*cell), Some(AnyValue::Null)) { return None; }
                        *cell = dict.any_key(*cell)?;
                    }
                }
                Some((cells, row))
            })
            .arrange_by_key();
        self.keyed.insert(id, arranged.clone());
        arranged
    }

    /// Two-input join; `project` names the columns of the concatenated row to emit, `None` all.
    fn join_rows(&mut self, cs: Vec<Coll<'s, T>>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>], project: Option<Vec<ColId>>) -> Result<Coll<'s, T>, EngineError> {
        if cs.len() != 2 {
            return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join arity != 2")));
        }
        let side = |input: u8| -> Vec<ColId> {
            eq.iter()
                .map(|class| class.iter().find(|(i, _)| *i == input).map(|(_, c)| *c).expect("check: class spans both inputs"))
                .collect()
        };
        let (lk, rk) = (side(0), side(1));
        let l_any = lk.iter().map(|c| types[0][*c as usize] == Ty::Any).collect::<Vec<_>>();
        let r_any = rk.iter().map(|c| types[1][*c as usize] == Ty::Any).collect::<Vec<_>>();
        let mut cs = cs.into_iter();
        let (l, r) = (cs.next().unwrap(), cs.next().unwrap());
        let l = self.keyed(l, lk, l_any);
        let r = self.keyed(r, rk, r_any);
        let left = types[0].len();
        Ok(l.join_core(r, move |_, a: &Row, b: &Row| {
            Some(match &project {
                None => {
                    let mut row = Vec::with_capacity(a.len() + b.len());
                    row.extend_from_slice(a);
                    row.extend_from_slice(b);
                    row
                }
                Some(project) => project.iter().map(|c| {
                    let c = *c as usize;
                    if c < left { a[c] } else { b[c - left] }
                }).collect(),
            })
        }))
    }
}

/// One record seen on an IR node's output collection in traced mode.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DdTap {
    pub node: NodeId,
    pub tick: u64,
    /// Relation identifying the enclosing LetRec scope.
    pub rec: Option<RelId>,
    /// Inner timestamp of the LetRec scope; `None` outside a loop.
    pub round: Option<u64>,
    pub row: Row,
    pub w: W,
}

fn cols(row: &[Cell], cols: &[ColId]) -> Row {
    cols.iter().map(|c| row[*c as usize]).collect()
}

impl<'s, T: Nest> Rel for DdRel<'s, T> {
    type C = Coll<'s, T>;

    fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError> {
        self.read.insert(rel);
        self.sources
            .get(&rel)
            .cloned()
            .ok_or_else(|| EngineError::new(Stage::Install, Some(rel), ErrorKind::UnknownRel(rel)))
    }

    fn mint(&mut self, c: Self::C, functor: RelId, args: &[ColId]) -> Result<Self::C, EngineError> {
        let (name, types) = self.constructors.get(&functor)
            .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
        if args.len() != types.len() {
            return Err(EngineError::new(Stage::Install, Some(functor), ErrorKind::Arity { expected: types.len(), actual: args.len() }));
        }
        let columns = args.to_vec();
        let interner = self.interner.clone();
        Ok(c.map(move |mut row| {
            let values = cols(&row, &columns);
            let id = interner.borrow_mut().mint(&name, &values, &types);
            row.push(id);
            row
        }))
    }

    fn decode(&mut self, c: Self::C, functor: RelId, col: ColId, _types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
        let (name, _) = self.constructors.get(&functor)
            .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
        let interner = self.interner.clone();
        Ok(c.flat_map(move |mut row| {
            let id = *row.get(col as usize)?;
            let dict = interner.borrow();
            let term = dict.get(id).filter(|term| *term.functor == *name)?;
            row.push(id);
            row.extend_from_slice(&term.args);
            Some(row)
        }))
    }

    fn str_cons(&mut self, c: Self::C, mode: &StrMode) -> Result<Self::C, EngineError> {
        let mode = mode.clone();
        let interner = self.interner.clone();
        Ok(c.flat_map(move |mut row| {
            match mode {
                StrMode::Construct { head, rest } => {
                    let joined = {
                        let dict = interner.borrow();
                        format!("{}{}", dict.text(row[head as usize])?, dict.text(row[rest as usize])?)
                    };
                    row.push(interner.borrow_mut().mint_text(&joined));
                }
                StrMode::Decompose { whole } => {
                    // Head and rest are minted here, when a row reads them; the empty string emits no row.
                    let (head, rest) = {
                        let dict = interner.borrow();
                        let text = dict.text(row[whole as usize])?;
                        let at = text.chars().next()?.len_utf8();
                        (text[..at].to_owned(), text[at..].to_owned())
                    };
                    let mut dict = interner.borrow_mut();
                    row.extend([dict.mint_text(&head), dict.mint_text(&rest)]);
                }
            }
            Some(row)
        }))
    }

    fn str_op(&mut self, c: Self::C, op: StrOp, args: &[ColId]) -> Result<Self::C, EngineError> {
        if args.len() != op.args().len() {
            return Err(EngineError::new(Stage::Install, None, ErrorKind::Arity { expected: op.args().len(), actual: args.len() }));
        }
        let columns = args.to_vec();
        let interner = self.interner.clone();
        Ok(c.flat_map(move |mut row| {
            let out = {
                let dict = interner.borrow();
                let mut values = Vec::with_capacity(columns.len());
                for (column, kind) in columns.iter().zip(op.args()) {
                    let cell = row[*column as usize];
                    values.push(match kind {
                        StrKind::Text => StrVal::Text(dict.text(cell)?),
                        StrKind::Int => StrVal::Int(cell),
                    });
                }
                op.apply(&values)?
            };
            match out {
                StrOut::Text(text) => row.push(interner.borrow_mut().mint_text(&text)),
                StrOut::Int(value) => row.push(value),
                StrOut::Holds => {}
            }
            Some(row)
        }))
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId], input_types: &[Ty]) -> Self::C {
        let (filter, map, project) = (filter.to_vec(), map.to_vec(), project.to_vec());
        let input_types = input_types.to_vec();
        let interner = self.interner.clone();
        let texts = self.texts.clone();
        c.flat_map(move |mut row: Row| {
            let lt = |a, b| interner.borrow().compare(a, b) == Ordering::Less;
            let literal = |index: u32| texts[index as usize];
            let nil = || interner.borrow().text_id("").expect("empty string");
            let compare = |ta: Ty, a: Cell, tb: Ty, b: Cell| {
                let numeric = |ty: Ty| matches!(ty, Ty::Int | Ty::Real);
                if numeric(ta) && numeric(tb) {
                    if ta == Ty::Int && tb == Ty::Int { return a.cmp(&b); }
                    let x = if ta == Ty::Real { f64::from_bits(a as u64) } else { a as f64 };
                    let y = if tb == Ty::Real { f64::from_bits(b as u64) } else { b as f64 };
                    return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
                }
                let rank = |ty: Ty| match ty { Ty::Int | Ty::Real => 1, Ty::Text => 2, Ty::Id => 3, Ty::Any => 4 };
                rank(ta).cmp(&rank(tb)).then_with(|| cmp_cell(ta, a, b, &interner.borrow()))
            };
            if !filter.iter().all(|e| eval_typed_with_text(e, &row, &input_types, &lt, &literal, &nil, &compare) != 0) {
                return None;
            }
            let mut types = input_types.clone();
            for e in &map {
                let v = eval_typed_with_text(e, &row, &types, &lt, &literal, &nil, &compare);
                types.push(expr_type(e, &types).expect("Mfp expression type"));
                row.push(v);
            }
            Some(if project.is_empty() { row } else { cols(&row, &project) })
        })
    }

    fn union(&mut self, cs: Vec<Self::C>) -> Self::C {
        let mut cs = cs.into_iter();
        let first = cs.next().expect("check rejects an empty Union");
        first.concatenate(cs)
    }

    fn negate(&mut self, c: Self::C) -> Self::C {
        c.negate()
    }

    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
        self.join_rows(cs, eq, types, None)
    }

    /// Unmeasured installs only: a measured install taps every IR node, the join included.
    fn fuses_join_project(&self) -> bool {
        self.taps.is_none()
    }

    fn join_project(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>], project: &[ColId]) -> Result<Self::C, EngineError> {
        self.join_rows(cs, eq, types, Some(project.to_vec()))
    }

    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C {
        let (lk, rk) = (lk.to_vec(), rk.to_vec());
        let keys = r
            .map(move |row| cols(&row, &rk))
            .threshold(|_, w: &W| if *w > 0 { 1 as W } else { 0 });
        l.map(move |row| (cols(&row, &lk), row)).antijoin(keys).map(|(_, row)| row)
    }

    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg], input_types: &[Ty]) -> Self::C {
        let (key, aggs) = (key.to_vec(), aggs.to_vec());
        let linear: Vec<Agg> = aggs.iter().filter(|a| matches!(a, Agg::Count | Agg::Sum(_))).cloned().collect();
        let extrema: Vec<Agg> = aggs.iter().filter(|a| matches!(a, Agg::Min(_) | Agg::Max(_))).cloned().collect();
        let extrema_types: Vec<Ty> = extrema.iter().map(|a| match a { Agg::Min(c) | Agg::Max(c) => input_types[*c as usize], _ => unreachable!() }).collect();
        let values = match (linear.is_empty(), extrema.is_empty()) {
            (false, true) => accumulable_values(c, key, linear, input_types.to_vec(), self.interner.clone(), self.sum_error.clone()),
            (true, false) => accumulable_values(c.clone(), key.clone(), vec![Agg::Count], input_types.to_vec(), self.interner.clone(), self.sum_error.clone())
                .join(hierarchical_extrema(c, key, extrema, extrema_types, self.interner.clone(), self.reduce_reads.clone()))
                .map(|(group, (_, values))| (group, values)),
            (false, false) => accumulable_values(c.clone(), key.clone(), linear, input_types.to_vec(), self.interner.clone(), self.sum_error.clone())
                .join(hierarchical_extrema(c, key, extrema, extrema_types, self.interner.clone(), self.reduce_reads.clone()))
                .map(move |(group, (linear, extrema))| {
                    let (mut li, mut ei) = (linear.into_iter(), extrema.into_iter());
                    let values = aggs.iter().map(|a| match a {
                        Agg::Count | Agg::Sum(_) => li.next().unwrap(),
                        Agg::Min(_) | Agg::Max(_) => ei.next().unwrap(),
                    }).collect();
                    (group, values)
                }),
            (true, true) => accumulable_values(c, key, linear, input_types.to_vec(), self.interner.clone(), self.sum_error.clone()),
        };
        values.map(|(mut group, values)| {
            group.extend(values);
            group
        })
    }

    fn threshold(&mut self, c: Self::C) -> Self::C {
        c.threshold(|_, w: &W| if *w > 0 { 1 as W } else { 0 })
    }

    fn topk(&mut self, c: Self::C, key: &[ColId], order: &[Order], limit: u32, input_types: &[Ty]) -> Result<Self::C, EngineError> {
        let (key, order) = (key.to_vec(), order.to_vec());
        let types = input_types.to_vec();
        let interner = self.interner.clone();
        Ok(c.map(move |row| (cols(&row, &key), row))
            .reduce(move |_k, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                let mut live: Vec<(&Row, W)> = input.iter().filter(|(_, w)| *w > 0).map(|(r, w)| (*r, *w)).collect();
                live.sort_by(|(a, _), (b, _)| rank(&order, &types, &interner.borrow(), a, b));
                let mut left = limit as W;
                for (row, w) in live {
                    if left == 0 {
                        break;
                    }
                    let take = w.min(left);
                    output.push((row.clone(), take));
                    left -= take;
                }
            })
            .map(|(_, row)| row))
    }

    fn window(&mut self, c: Self::C, partition: &[ColId], order: &[Order], func: &WinFn, input_types: &[Ty]) -> Result<Self::C, EngineError> {
        let (partition, order, func, types) = (partition.to_vec(), order.to_vec(), func.clone(), input_types.to_vec());
        let interner = self.interner.clone();
        let value_col = order.first().map_or(0, |o| o.col as usize);
        Ok(c.map(move |row| (cols(&row, &partition), row))
            .reduce(move |_key, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                let mut rows = Vec::new();
                for (row, weight) in input {
                    for _ in 0..(*weight).max(0) {
                        rows.push((*row).clone());
                    }
                }
                rows.sort_by(|a, b| rank(&order, &types, &interner.borrow(), a, b));
                let total_sum = if let WinFn::Sum(col) = func {
                    rows.iter().fold(0i64, |sum, row| sum.wrapping_add(row[col as usize]))
                } else { 0 };
                let mut values: BTreeMap<Row, W> = BTreeMap::new();
                let mut dense = 0i64;
                let mut rank_at = 0i64;
                let mut prefix_sum = 0i64;
                for (i, row) in rows.iter().enumerate() {
                    if i == 0 || order.iter().any(|o| row[o.col as usize] != rows[i - 1][o.col as usize]) {
                        dense += 1;
                        rank_at = i as i64 + 1;
                    }
                    let value = match func {
                        WinFn::RowNumber => i as i64 + 1,
                        WinFn::Rank => rank_at,
                        WinFn::DenseRank => dense,
                        WinFn::Lag(offset) => i.checked_sub(offset as usize).map_or(0, |j| rows[j][value_col]),
                        WinFn::Lead(offset) => rows.get(i + offset as usize).map_or(0, |r| r[value_col]),
                        WinFn::Sum(col) => {
                            prefix_sum = prefix_sum.wrapping_add(row[col as usize]);
                            if order.is_empty() { total_sum } else { prefix_sum }
                        }
                        WinFn::Count => if order.is_empty() { rows.len() as i64 } else { i as i64 + 1 },
                    };
                    let mut result = row.clone();
                    result.push(value);
                    *values.entry(result).or_default() += 1;
                }
                output.extend(values);
            })
            .map(|(_, row)| row))
    }

    fn letrec(&mut self, p: &Program, rec: &LetRec, defined: &[(RelId, Self::C)]) -> Result<Vec<Self::C>, EngineError> {
        T::letrec(self, p, rec, defined)
    }

    fn node_types(&mut self, p: &Program, id: NodeId) -> Option<Vec<Ty>> {
        p.node_types_memo(id, &mut self.types.borrow_mut())
    }

    fn output(&mut self, rel: RelId, c: Self::C) {
        self.outputs.push((rel, c));
    }

    fn observe(&mut self, id: NodeId, c: Self::C) -> Self::C {
        let Some(sink) = &self.taps else { return c };
        let sink = Rc::clone(sink);
        let rec = self.rec;
        c.inspect(move |(row, t, w)| {
            let (tick, round) = t.split();
            sink.borrow_mut().push(DdTap { node: id, tick, rec, round, row: row.clone(), w: *w });
        })
    }
}

/// Count and Sum ride in the diff (`[count, sums..]`), so a group is one record and a change costs O(1) in group size.
fn numeric_prefix(bytes: &[u8]) -> f64 {
    let mut end = 0;
    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) { end += 1; }
    if end < bytes.len() && matches!(bytes[end], b'+' | b'-') { end += 1; }
    let mut digits = 0;
    while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; digits += 1; }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; digits += 1; }
    }
    if digits == 0 { return 0.0; }
    let exponent = end;
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        end += 1;
        if end < bytes.len() && matches!(bytes[end], b'+' | b'-') { end += 1; }
        let before = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
        if end == before { end = exponent; }
    }
    std::str::from_utf8(&bytes[..end]).ok().and_then(|text| text.trim().parse::<f64>().ok()).unwrap_or(0.0)
}

fn sum_add(sum: &mut f64, error: &mut f64, value: f64) {
    let old = *sum;
    let next = old + value;
    if old.abs() > value.abs() { *error += (old - next) + value; }
    else { *error += (value - next) + old; }
    *sum = next;
}
fn sum_add_integer(sum: &mut f64, error: &mut f64, value: i64) {
    if !(-4503599627370495..=4503599627370495).contains(&value) {
        let small = value % 16384;
        sum_add(sum, error, (value - small) as f64);
        sum_add(sum, error, small as f64);
    } else { sum_add(sum, error, value as f64); }
}
fn sum_init(integer: i64) -> (f64, f64) {
    if !(-4503599627370495..=4503599627370495).contains(&integer) {
        let small = integer % 16384;
        ((integer - small) as f64, small as f64)
    } else { (integer as f64, 0.0) }
}

fn sqlite_sum_any(values: &[(AnyValue, W)]) -> Result<AnyValue, &'static str> {
    let (mut integer, mut real, mut error, mut approximate, mut overflow, mut seen) = (0i64, 0.0f64, 0.0f64, false, false, false);
    for (value, weight) in values {
        for _ in 0..(*weight).max(0) {
            let numeric = match value {
                AnyValue::Null => continue,
                AnyValue::Integer(value) => Ok(*value),
                AnyValue::Text(value) => value.trim().parse::<i64>().map_err(|_| numeric_prefix(value.as_bytes())),
                AnyValue::Real(bits) => Err(f64::from_bits(*bits)),
                AnyValue::Blob(value) => Err(numeric_prefix(value)),
            };
            match numeric {
                Ok(value) if !approximate => {
                    if let Some(next) = integer.checked_add(value) { integer = next; }
                    else {
                        overflow = true;
                        (real, error) = sum_init(integer);
                        approximate = true;
                        sum_add_integer(&mut real, &mut error, value);
                    }
                }
                Ok(value) => sum_add_integer(&mut real, &mut error, value),
                Err(value) => {
                    if !approximate { (real, error) = sum_init(integer); approximate = true; }
                    overflow = false;
                    sum_add(&mut real, &mut error, value);
                }
            }
            seen = true;
        }
    }
    if overflow { return Err("integer overflow"); }
    let total = if error.is_infinite() { real } else { real + error };
    Ok(if !seen || approximate && total.is_nan() { AnyValue::Null }
        else if approximate { AnyValue::Real(total.to_bits()) }
        else { AnyValue::Integer(integer) })
}

fn accumulable_values<'s, T: Nest>(c: Coll<'s, T>, key: Vec<ColId>, aggs: Vec<Agg>, types: Vec<Ty>, interner: Rc<RefCell<Interner>>, sum_error: Rc<RefCell<Option<String>>>) -> VecCollection<'s, T, (Row, Row), W> {
    let key_cols = key.clone();
    let key_types = types.clone();
    let sum_interner = interner.clone();
    let key_of = move |row: &Row| {
        let mut group = cols(row, &key_cols);
        for (cell, col) in group.iter_mut().zip(&key_cols) {
            if key_types[*col as usize] == Ty::Any {
                *cell = interner.borrow().any_key(*cell).expect("Any cell");
            }
        }
        group
    };
    let real_sum = aggs.iter().any(|agg| matches!(agg, Agg::Sum(col) if types[*col as usize] == Ty::Real));
    if key.iter().any(|col| types[*col as usize] == Ty::Any)
        || aggs.iter().any(|agg| matches!(agg, Agg::Sum(col) if types[*col as usize] == Ty::Any)) {
        let key_len = key.len();
        return c.map(move |row| (key_of(&row), row))
            .reduce(move |_group, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                let count: W = input.iter().map(|(_, w)| *w).sum();
                if count <= 0 { return; }
                let representative = input.iter().filter(|(_, w)| *w > 0)
                    .min_by_key(|(row, _)| cols(row, &key))
                    .expect("live group").0;
                let mut values = cols(representative, &key);
                for agg in &aggs {
                    values.push(match agg {
                        Agg::Count => count,
                        Agg::Sum(col) if types[*col as usize] == Ty::Real => input.iter()
                            .map(|(row, w)| f64::from_bits(row[*col as usize] as u64) * *w as f64)
                            .sum::<f64>().to_bits() as i64,
                        Agg::Sum(col) if types[*col as usize] == Ty::Any => {
                            let values = input.iter().map(|(row, w)| {
                                (sum_interner.borrow().any_value(row[*col as usize]).expect("Any cell").clone(), *w)
                            }).collect::<Vec<_>>();
                            let sum = match sqlite_sum_any(&values) {
                                Ok(sum) => sum,
                                Err(error) => {
                                    *sum_error.borrow_mut() = Some(error.to_string());
                                    AnyValue::Null
                                }
                            };
                            sum_interner.borrow_mut().mint_any(&sum)
                        }
                        Agg::Sum(col) => input.iter().fold(0i64, |sum, (row, w)| sum.wrapping_add(row[*col as usize].wrapping_mul(*w))),
                        _ => unreachable!(),
                    });
                }
                output.push((values, 1));
            })
            .map(move |(_, row)| (row[..key_len].to_vec(), row[key_len..].to_vec()));
    }
    if real_sum {
        return c.map(move |row| (key_of(&row), row))
            .reduce(move |_key, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                let count: W = input.iter().map(|(_, w)| *w).sum();
                if count <= 0 { return; }
                let values = aggs.iter().map(|agg| match agg {
                    Agg::Count => count,
                    Agg::Sum(col) if types[*col as usize] == Ty::Real => {
                        input.iter().map(|(row, w)| f64::from_bits(row[*col as usize] as u64) * *w as f64)
                            .sum::<f64>().to_bits() as i64
                    }
                    Agg::Sum(col) => input.iter().fold(0i64, |sum, (row, w)| sum.wrapping_add(row[*col as usize].wrapping_mul(*w))),
                    _ => unreachable!(),
                }).collect();
                output.push((values, 1));
            });
    }
    let sums: Vec<ColId> = aggs.iter().filter_map(|a| if let Agg::Sum(c) = a { Some(*c) } else { None }).collect();
    c.explode(move |row: Row| {
        let mut acc = vec![1 as W];
        acc.extend(sums.iter().map(|c| row[*c as usize]));
        Some(((key_of(&row), ()), acc))
    })
    .reduce(move |_k, input: &[(&(), Vec<W>)], output: &mut Vec<(Row, W)>| {
        let acc = &input[0].1;
        if acc[0] <= 0 {
            return;
        }
        let mut next_sum = 1;
        let values = aggs
            .iter()
            .map(|a| match a {
                Agg::Count => acc[0],
                _ => {
                    next_sum += 1;
                    acc[next_sum - 1]
                }
            })
            .collect();
        output.push((values, 1));
    })
}

/// Each level replaces up to 16 child buckets with their extrema. The final group reduce
/// sees only the 16 possible high hash nibbles, regardless of the group's row count.
fn hierarchical_extrema<'s, T: Nest>(
    c: Coll<'s, T>, key: Vec<ColId>, aggs: Vec<Agg>, types: Vec<Ty>, interner: Rc<RefCell<Interner>>, reduce_reads: Option<ReduceReadLogger>,
) -> VecCollection<'s, T, (Row, Row), W> {
    let columns: Vec<ColId> = aggs.iter().map(|a| match a { Agg::Min(c) | Agg::Max(c) => *c, _ => unreachable!() }).collect();
    // Test row liveness before collapsing rows with equal aggregate values.
    let mut buckets = c.threshold(|_, w: &W| if *w > 0 { 1 } else { 0 })
        .map(move |row| (cols(&row, &key), cols(&row, &columns)))
        .threshold(|_, w: &W| if *w > 0 { 1 } else { 0 })
        .map(move |(group, values)| {
            let mut hasher = DefaultHasher::new();
            values.hash(&mut hasher);
            ((group, hasher.finish()), values)
        });
    for _ in 0..15 {
        let level_aggs = aggs.clone();
        let level_types = types.clone();
        let level_interner = interner.clone();
        let level_reads = reduce_reads.clone();
        buckets = buckets
            .reduce(move |_, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                if let Some(logger) = &level_reads { logger.log(input.len()); }
                extrema(input, &level_aggs, &level_types, &level_interner.borrow(), output)
            })
            .map(|((group, bucket), values)| ((group, bucket >> 4), values));
    }
    let level_aggs = aggs.clone();
    let level_types = types.clone();
    let level_interner = interner.clone();
    let level_reads = reduce_reads.clone();
    buckets
        .reduce(move |_, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
            if let Some(logger) = &level_reads { logger.log(input.len()); }
            extrema(input, &level_aggs, &level_types, &level_interner.borrow(), output)
        })
        .map(|((group, _), values)| (group, values))
        .reduce(move |_, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
            if let Some(logger) = &reduce_reads { logger.log(input.len()); }
            extrema(input, &aggs, &types, &interner.borrow(), output)
        })
}

fn extrema(input: &[(&Row, W)], aggs: &[Agg], types: &[Ty], interner: &Interner, output: &mut Vec<(Row, W)>) {
    let mut live = input.iter().filter(|(_, w)| *w > 0).map(|(row, _)| *row).peekable();
    if live.peek().is_none() { return; }
    let values = aggs.iter().enumerate().map(|(i, agg)| match agg {
        Agg::Min(_) => live.clone().map(|row| row[i])
            .filter(|cell| types[i] != Ty::Any || !matches!(interner.any_value(*cell), Some(AnyValue::Null)))
            .min_by(|a, b| cmp_cell(types[i], *a, *b, interner))
            .unwrap_or_else(|| live.clone().next().unwrap()[i]),
        Agg::Max(_) => live.clone().map(|row| row[i])
            .filter(|cell| types[i] != Ty::Any || !matches!(interner.any_value(*cell), Some(AnyValue::Null)))
            .max_by(|a, b| cmp_cell(types[i], *a, *b, interner))
            .unwrap_or_else(|| live.clone().next().unwrap()[i]),
        _ => unreachable!(),
    }).collect();
    output.push((values, 1));
}

/// `order` first, then the whole row ascending, so ties resolve the same way in every engine.
fn cmp_cell(ty: Ty, a: Cell, b: Cell, interner: &Interner) -> Ordering {
    match ty {
        Ty::Id => interner.compare(a, b),
        Ty::Text => interner.text(a).cmp(&interner.text(b)),
        Ty::Real => f64::from_bits(a as u64).partial_cmp(&f64::from_bits(b as u64)).unwrap_or_else(|| a.cmp(&b)),
        Ty::Any => interner.any_value(a).expect("Any cell").sqlite_cmp(interner.any_value(b).expect("Any cell")),
        Ty::Int => a.cmp(&b),
    }
}

fn rank(order: &[Order], types: &[Ty], interner: &Interner, a: &Row, b: &Row) -> Ordering {
    order
        .iter()
        .map(|o| {
            let i = o.col as usize;
            let ord = cmp_cell(types[i], a[i], b[i], interner);
            if o.desc { ord.reverse() } else { ord }
        })
        .find(|ord| ord.is_ne())
        .unwrap_or_else(|| a.iter().zip(b).enumerate().map(|(i, (x, y))| cmp_cell(types[i], *x, *y, interner)).find(|o| o.is_ne()).unwrap_or(Ordering::Equal))
}

enum Command {
    Settle(Frontier, mpsc::Sender<Result<(Delta, Vec<DdTap>, Counters), EngineError>>),
    Snapshot(RelId, mpsc::Sender<Result<Vec<(Row, W)>, EngineError>>),
    InternSnapshot(RelId, mpsc::Sender<Result<Vec<(Row, W)>, EngineError>>),
    InternSnapshots(Vec<RelId>, mpsc::Sender<Result<Vec<Vec<(Row, W)>>, EngineError>>),
    InternTerms(Vec<(RelId, Row)>, mpsc::Sender<Result<Vec<Cell>, EngineError>>),
    InternText(String, mpsc::Sender<Cell>),
    Text(Cell, mpsc::Sender<Option<String>>),
    InternAny(AnyValue, mpsc::Sender<Cell>),
    AnyValue(Cell, mpsc::Sender<Option<AnyValue>>),
    Stop,
}

pub struct Dd {
    tx: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    counters: Counters,
    /// Unmeasured: `drop` does not wait for the worker to free the dataflow.
    detach: bool,
}

/// What the worker records per settle. `Measured` and `Traced` tap every IR node's output to
/// fill `Counters::delta_rows` and `Counters::rounds`; `Traced` also returns the taps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Unmeasured,
    Measured,
    Traced,
}

/// Runs on the worker before the dataflow is built; tests register timely/differential loggers here.
pub type Hook = Box<dyn FnOnce(&mut timely::worker::Worker) + Send>;

impl Dd {
    pub fn install(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, Mode::Measured)
    }

    pub fn install_observed(program: &Program, hook: Hook) -> Result<Self, EngineError> {
        Self::start(program, Some(hook), Mode::Measured)
    }

    /// Every IR node's output collection gets an `inspect`; `settle_traced` returns what they saw.
    pub fn install_traced(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, Mode::Traced)
    }

    /// `settle` plus the node records of this step, consolidated per `(node, tick, round, row)`.
    /// Empty unless installed with `install_traced`.
    pub fn settle_traced(&mut self, frontier: Frontier) -> Result<(Delta, Vec<DdTap>), EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Settle(frontier, reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        let (delta, taps, counters) = answer.recv().map_err(|e| worker_error(Stage::Settle, e))??;
        self.counters = counters;
        Ok((delta, taps))
    }

    fn start(program: &Program, hook: Option<Hook>, mode: Mode) -> Result<Self, EngineError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let program = program.clone();
        let thread = thread::spawn(move || worker(program, hook, mode, rx, ready_tx));
        let worker_gone = |_| EngineError::new(Stage::Install, None, ErrorKind::Worker("worker exited".into()));
        ready_rx.recv().map_err(worker_gone)??;
        Ok(Self { tx, thread: Some(thread), counters: Counters::default(), detach: mode == Mode::Unmeasured })
    }
}

impl Engine for Dd {
    fn install(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, Mode::Measured)
    }

    /// No node taps: every IR node's output is not inspected, cloned and consolidated per settle.
    fn install_unmeasured(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, Mode::Unmeasured)
    }

    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        self.settle_traced(frontier).map(|(delta, _)| delta)
    }

    fn counters(&self) -> Counters { self.counters }

    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Snapshot(rel, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
    }

    fn intern_snapshot(&self, functor: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternSnapshot(functor, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
    }
    /// One worker round trip and one pass over the dictionary for every functor.
    fn intern_snapshots(&self, functors: &[RelId]) -> Result<Vec<Vec<(Row, W)>>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternSnapshots(functors.to_vec(), reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
    }
    fn intern_terms(&mut self, terms: &[(RelId, Row)]) -> Result<Vec<Cell>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternTerms(terms.to_vec(), reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Settle, e))?
    }
    fn intern_text(&mut self, value: &str) -> Result<Cell, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternText(value.to_owned(), reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Settle, e))
    }
    fn text(&self, id: Cell) -> Result<Option<String>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Text(id, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))
    }
    fn intern_any(&mut self, value: &AnyValue) -> Result<Cell, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternAny(value.clone(), reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Settle, e))
    }
    fn any_value(&self, id: Cell) -> Result<AnyValue, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::AnyValue(id, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
            .ok_or_else(|| EngineError::new(Stage::Snapshot, None, ErrorKind::Worker(format!("unknown Any cell {id}"))))
    }
}

fn worker_error(stage: Stage, e: impl std::fmt::Display) -> EngineError {
    EngineError::new(stage, None, ErrorKind::Worker(e.to_string()))
}

impl Drop for Dd {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            if !self.detach {
                let _ = thread.join();
            }
        }
    }
}

struct Built {
    inputs: BTreeMap<RelId, InputSession<Time, Row, W>>,
    constructor_inputs: BTreeMap<RelId, InputSession<Time, Row, W>>,
    /// Each output's rows, accumulated from the settled deltas; `Snapshot` reads them.
    outputs: BTreeMap<RelId, BTreeMap<Row, W>>,
}

fn worker(program: Program, hook: Option<Hook>, mode: Mode, rx: mpsc::Receiver<Command>, ready: mpsc::Sender<Result<(), EngineError>>) {
    let rx = std::sync::Mutex::new(rx);
    let hook = std::sync::Mutex::new(hook);
    timely::execute_directly(move |worker| {
        if let Some(hook) = hook.lock().unwrap().take() {
            hook(worker);
        }
        let reduce_reads = worker.log_register().and_then(|registry| registry.get::<ReduceReadEventBuilder>("lab/reduce_reads"));
        let mut probe = ProbeHandle::new();
        let captured: Rc<RefCell<Vec<(RelId, Row, Time, W)>>> = Rc::default();
        let taps: Option<Rc<RefCell<Vec<DdTap>>>> = (mode != Mode::Unmeasured).then(Rc::default);
        let interner = Rc::new(RefCell::new(Interner::default()));
        let sum_error = Rc::new(RefCell::new(None));
        let texts = {
            let mut dict = interner.borrow_mut();
            if program.uses_strings() { dict.mint_text(""); }
            program.texts.iter().map(|text| dict.mint_text(text)).collect::<Vec<_>>()
        };
        let constructors: BTreeMap<RelId, (String, Vec<Ty>)> = program.rels.iter()
            .filter(|r| r.kind == RelKind::Constructor)
            .map(|r| (r.id, (r.name.clone(), r.cols.iter().skip(1).copied().collect())))
            .collect();
        let mut constructor_ids: std::collections::HashMap<String, RelId> = std::collections::HashMap::new();
        for (id, (name, _)) in &constructors {
            constructor_ids.entry(name.clone()).or_insert(*id);
        }
        let build_start = std::time::Instant::now();
        let arranged: Rc<RefCell<ArrangeStats>> = Rc::default();
        let built = worker.dataflow::<Time, _, _>(|scope| -> Result<Built, EngineError> {
            let mut inputs = BTreeMap::new();
            let mut constructor_inputs = BTreeMap::new();
            let mut sources = BTreeMap::new();
            for rel in program.rels.iter().filter(|r| r.kind == RelKind::Source) {
                let (input, c) = scope.new_collection::<Row, W>();
                inputs.insert(rel.id, input);
                sources.insert(rel.id, c);
            }
            for rel in program.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
                let (input, c) = scope.new_collection::<Row, W>();
                c.clone().probe_with(&mut probe);
                constructor_inputs.insert(rel.id, input);
                sources.insert(rel.id, c);
            }
            let mut rel = DdRel { scope, rec: None, sources, outputs: Vec::new(), taps: taps.clone(), reduce_reads: reduce_reads.clone(), constructors: constructors.clone(), interner: interner.clone(), sum_error: sum_error.clone(), texts: texts.clone(), keyed: BTreeMap::new(), read: BTreeSet::new(), types: Rc::default(), arranged: arranged.clone() };
            lower(&program, &mut rel)?;
            // A constructor no operator reads gets no rows: its terms would cost an epoch and drive nothing.
            constructor_inputs.retain(|id, _| rel.read.contains(id));
            let mut outputs = BTreeMap::new();
            for (id, c) in rel.outputs {
                let sink = Rc::clone(&captured);
                c.inspect(move |(row, t, w)| sink.borrow_mut().push((id, row.clone(), *t, *w)))
                    .probe_with(&mut probe);
                outputs.insert(id, BTreeMap::new());
            }
            Ok(Built { inputs, constructor_inputs, outputs })
        });
        tracing::debug!(target: "ivm_dd", operators = worker.peek_identifier(), nodes = program.nodes.len(), build_ns = build_start.elapsed().as_nanos() as u64, arrangements = arranged.borrow().arrangements, "dd install");
        let mut built = match built {
            Ok(built) => {
                let _ = ready.send(Ok(()));
                built
            }
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        let mut source_rows: BTreeMap<RelId, HashSet<Row>> = program.rels.iter()
            .filter(|rel| rel.kind == RelKind::Source)
            .map(|rel| (rel.id, HashSet::new())).collect();
        let mut epoch: Time = 0;
        let mut frontier_tick = 0;
        loop {
            let Ok(command) = rx.lock().unwrap().recv() else { break };
            match command {
                Command::Settle(frontier, reply) => {
                    let settle_start = std::time::Instant::now();
                    let (epoch_before, mut steps) = (epoch, 0u64);
                    let changes_in = frontier.changes.len();
                    let interned_before = interner.borrow().len();
                    let accepted = match guard(&program, &mut source_rows, epoch, &frontier) {
                        Ok(accepted) => accepted,
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            continue;
                        }
                    };
                    for change in accepted {
                        built.inputs.get_mut(&change.rel).unwrap().update(change.row, change.w);
                    }
                    // Terms interned since the last settle enter with the frontier.
                    for (name, row) in interner.borrow_mut().drain_pending() {
                        if let Some(input) = constructor_ids.get(&*name).and_then(|id| built.constructor_inputs.get_mut(id)) {
                            input.update(row, 1);
                        }
                    }
                    epoch += 1;
                    for input in built.inputs.values_mut() {
                        input.advance_to(epoch);
                        input.flush();
                    }
                    for input in built.constructor_inputs.values_mut() {
                        input.advance_to(epoch);
                        input.flush();
                    }
                    worker.step_while(|| {
                        steps += 1;
                        probe.less_than(&epoch)
                    });
                    if let Some(message) = sum_error.borrow_mut().take() {
                        let _ = reply.send(Err(EngineError::new(Stage::Settle, None, ErrorKind::Worker(message))));
                        break;
                    }
                    loop {
                        let mut fed = false;
                        for (name, row) in interner.borrow_mut().drain_pending() {
                            if let Some(input) = constructor_ids.get(&*name).and_then(|id| built.constructor_inputs.get_mut(id)) {
                                input.update(row, 1);
                                fed = true;
                            }
                        }
                        if !fed { break; }
                        epoch += 1;
                        for input in built.inputs.values_mut() {
                            input.advance_to(epoch);
                            input.flush();
                        }
                        for input in built.constructor_inputs.values_mut() {
                            input.advance_to(epoch);
                            input.flush();
                        }
                        worker.step_while(|| {
                            steps += 1;
                            probe.less_than(&epoch)
                        });
                    }
                    if let Some(logger) = &reduce_reads { logger.flush(); }
                    let mut net: BTreeMap<(RelId, Row), W> = BTreeMap::new();
                    for (rel, row, _, w) in captured.borrow_mut().drain(..) {
                        *net.entry((rel, row)).or_default() += w;
                    }
                    let changes: Vec<_> = net.into_iter()
                        .filter(|(_, w)| *w != 0)
                        .map(|((rel, row), w)| (rel, row, w))
                        .collect();
                    for (rel, row, w) in &changes {
                        let rows = built.outputs.get_mut(rel).expect("captured rows name an output");
                        let total = rows.entry(row.clone()).or_default();
                        *total += w;
                        if *total == 0 {
                            rows.remove(row);
                        }
                    }
                    let mut seen: BTreeMap<(NodeId, u64, Option<RelId>, Option<u64>, Row), W> = BTreeMap::new();
                    for tap in taps.iter().flat_map(|t| t.borrow_mut().drain(..).collect::<Vec<_>>()) {
                        *seen.entry((tap.node, frontier_tick, tap.rec, tap.round, tap.row)).or_default() += tap.w;
                    }
                    let seen: Vec<DdTap> = seen
                        .into_iter()
                        .filter(|(_, w)| *w != 0)
                        .map(|((node, tick, rec, round, row), w)| DdTap { node, tick, rec, round, row, w })
                        .collect();
                    let mut counters = if taps.is_some() { Counters::measured() } else { Counters::default() };
                    counters.statements = None;
                    counters.rows_written = changes.len() as u64;
                    counters.interned = Some((interner.borrow().len() - interned_before) as u64);
                    let mut rounds = std::collections::BTreeSet::new();
                    for tap in &seen {
                        if let (Some(rec), Some(round)) = (tap.rec, tap.round) { rounds.insert((rec, round)); }
                        let field = match program.nodes.get(tap.node as usize) {
                            Some(Op::Mfp { .. }) => &mut counters.delta_rows.filter,
                            Some(Op::Join { .. }) => &mut counters.delta_rows.join,
                            Some(Op::Antijoin { .. }) => &mut counters.delta_rows.antijoin,
                            Some(Op::Reduce { .. }) => &mut counters.delta_rows.reduce,
                            Some(Op::TopK { .. }) => &mut counters.delta_rows.topk,
                            Some(Op::Window { .. }) => &mut counters.delta_rows.window,
                            Some(Op::Mint { .. } | Op::StrCons { .. } | Op::Str { .. }) => &mut counters.delta_rows.mint,
                            _ => continue,
                        };
                        *field.as_mut().unwrap() += 1;
                    }
                    if taps.is_some() {
                        counters.rounds = Some(rounds.into_iter().filter(|(_, round)| *round > 0).count() as u64);
                    }
                    tracing::debug!(target: "ivm_dd", changes_in, changes_out = changes.len(), epochs = epoch - epoch_before, steps, settle_ns = settle_start.elapsed().as_nanos() as u64, arranged_cells = arranged.borrow().cells, "dd settle");
                    let _ = reply.send(Ok((Delta { tick: frontier_tick, changes }, if mode == Mode::Traced { seen } else { Vec::new() }, counters)));
                    frontier_tick += 1;
                }
                Command::Snapshot(rel, reply) => {
                    let answer = built
                        .outputs
                        .get(&rel)
                        .map(|rows| rows.iter().map(|(row, w)| (row.clone(), *w)).collect())
                        .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)));
                    let _ = reply.send(answer);
                }
                Command::InternSnapshot(rel, reply) => {
                    let answer = constructors.get(&rel)
                        .map(|(name, _)| interner.borrow().snapshot(name))
                        .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)));
                    let _ = reply.send(answer);
                }
                Command::InternSnapshots(rels, reply) => {
                    let answer = rels.iter()
                        .map(|rel| constructors.get(rel).map(|(name, _)| name.as_str())
                            .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(*rel), ErrorKind::UnknownRel(*rel))))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|names| interner.borrow().snapshots(&names));
                    let _ = reply.send(answer);
                }
                Command::InternTerms(terms, reply) => {
                    let answer = terms.iter().map(|(functor, args)| {
                        let (name, types) = constructors.get(functor)
                            .ok_or_else(|| EngineError::new(Stage::Settle, Some(*functor), ErrorKind::UnknownRel(*functor)))?;
                        if args.len() != types.len() {
                            return Err(EngineError::new(Stage::Settle, Some(*functor), ErrorKind::Arity { expected: types.len(), actual: args.len() }));
                        }
                        Ok(interner.borrow_mut().mint(name, args, types))
                    }).collect();
                    let _ = reply.send(answer);
                }
                Command::InternText(value, reply) => { let _ = reply.send(interner.borrow_mut().mint_text(&value)); }
                Command::Text(id, reply) => { let _ = reply.send(interner.borrow().text(id).map(str::to_owned)); }
                Command::InternAny(value, reply) => { let _ = reply.send(interner.borrow_mut().mint_any(&value)); }
                Command::AnyValue(id, reply) => { let _ = reply.send(interner.borrow().any_value(id).cloned()); }
                Command::Stop => break,
            }
        }
        if mode == Mode::Unmeasured {
            // Nothing reads the dataflow again: free it without stepping it to completion.
            drop(built);
            for dataflow in worker.installed_dataflows() {
                worker.drop_dataflow(dataflow);
            }
        }
    });
}

/// Source relations are sets. Insert of a present row rejects the frontier; delete of an
/// absent row is a no-op, like SQL `DELETE` matching nothing, and logs a warning.
fn guard(
    program: &Program,
    sources: &mut BTreeMap<RelId, HashSet<Row>>,
    tick: Time,
    frontier: &Frontier,
) -> Result<Vec<SourceChange>, EngineError> {
    let mut pending: BTreeMap<(RelId, Row), W> = BTreeMap::new();
    let mut accepted = Vec::new();
    for change in &frontier.changes {
        let rel = program
            .rel(change.rel)
            .filter(|r| r.kind == RelKind::Source)
            .ok_or_else(|| EngineError::new(Stage::Settle, Some(change.rel), ErrorKind::UnknownRel(change.rel)))?;
        if change.row.len() != rel.cols.len() {
            let kind = ErrorKind::Arity { expected: rel.cols.len(), actual: change.row.len() };
            return Err(EngineError::new(Stage::Settle, Some(rel.id), kind));
        }
        if change.w != 1 && change.w != -1 {
            return Err(EngineError::new(Stage::Settle, Some(rel.id), ErrorKind::Unsupported("weight other than +1/-1")));
        }
        let key = (rel.id, change.row.clone());
        let before = *pending.get(&key).unwrap_or(&0)
            + W::from(sources.get(&rel.id).unwrap().contains(&change.row));
        if change.w > 0 && before > 0 {
            return Err(EngineError::new(Stage::Settle, Some(rel.id), ErrorKind::PresentInsert(change.row.clone())));
        }
        if change.w < 0 && before <= 0 {
            tracing::warn!(tick, relation = %rel.name, row = ?change.row, "delete of absent row ignored");
            continue;
        }
        *pending.entry(key).or_default() += change.w;
        accepted.push(change.clone());
    }
    for change in &accepted {
        let rows = sources.get_mut(&change.rel).unwrap();
        if change.w > 0 { rows.insert(change.row.clone()); }
        else { rows.remove(&change.row); }
    }
    Ok(accepted)
}
