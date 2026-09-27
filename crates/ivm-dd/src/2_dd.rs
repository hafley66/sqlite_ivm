//! differential-dataflow engine: `lower` runs once inside `worker.dataflow`; each `settle` is one epoch.

use ivm_ir::*;
use ivm_engine::*;
use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::arrange::TraceAgent;
use differential_dataflow::trace::cursor::Cursor;
use differential_dataflow::trace::implementations::KeySpine;
use differential_dataflow::trace::TraceReader;
use differential_dataflow::lattice::Lattice;
use differential_dataflow::operators::iterate::Variable;
use differential_dataflow::VecCollection;
use std::hash::{DefaultHasher, Hash, Hasher};
use timely::dataflow::Scope;
use timely::order::Product;
use timely::progress::Timestamp;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::dataflow::operators::Probe;
use timely::progress::frontier::AntichainRef;
use std::time::Duration;

type Time = u64;
type Coll<'s, T = Time> = VecCollection<'s, T, Row, W>;
type Trace = TraceAgent<KeySpine<Row, Time, W>>;
type Inner = Product<Time, u64>;
/// Rows supplied by an arrangement to a hierarchical reduce closure.
pub type ReduceReadEventBuilder = timely::container::CapacityContainerBuilder<Vec<(Duration, usize)>>;
type ReduceReadLogger = timely::logging_core::Logger<ReduceReadEventBuilder>;

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
        if rec.limit.is_some() {
            return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("LetRec limit")));
        }
        let outer = rel.scope;
        outer.scoped::<Inner, _, _>("LetRec", |sub| {
            let mut inner = DdRel {
                scope: sub,
                rec: rec.ids.first().copied(),
                sources: rel.sources.iter().map(|(id, c)| (*id, c.clone().enter(sub))).collect(),
                outputs: Vec::new(),
                taps: rel.taps.clone(),
                reduce_reads: rel.reduce_reads.clone(),
                constructors: rel.constructors.clone(),
                interner: rel.interner.clone(),
                texts: rel.texts.clone(),
            };
            let mut scope_defined: Vec<(RelId, Coll<'_, Inner>)> =
                defined.iter().map(|(id, c)| (*id, c.clone().enter(sub))).collect();
            let mut variables = Vec::new();
            for id in &rec.ids {
                let (variable, current) = Variable::new(sub, Product::new(Default::default(), 1));
                variables.push(variable);
                scope_defined.push((*id, current));
            }
            let mut nodes = vec![None; p.nodes.len()];
            let mut results = Vec::new();
            for body in &rec.bodies {
                let c = lower_node(p, &mut inner, &mut nodes, &scope_defined, *body)?;
                results.push(inner.threshold(c));
            }
            let mut left = Vec::new();
            for (variable, result) in variables.into_iter().zip(results) {
                variable.set(result.clone());
                left.push(result.leave(outer));
            }
            Ok(left)
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
    texts: Vec<Cell>,
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
                    let (head, rest) = interner.borrow().split(row[whole as usize])?;
                    row.extend([head, rest]);
                }
            }
            Some(row)
        }))
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C {
        let (filter, map, project) = (filter.to_vec(), map.to_vec(), project.to_vec());
        let interner = self.interner.clone();
        let texts = self.texts.clone();
        c.flat_map(move |mut row: Row| {
            let lt = |a, b| interner.borrow().compare(a, b) == Ordering::Less;
            let literal = |index: u32| texts[index as usize];
            let nil = || interner.borrow().text_id("").expect("empty string");
            if !filter.iter().all(|e| eval_with_text(e, &row, &lt, &literal, &nil) != 0) {
                return None;
            }
            for e in &map {
                let v = eval_with_text(e, &row, &lt, &literal, &nil);
                row.push(v);
            }
            Some(if project.is_empty() { row } else { cols(&row, &project) })
        })
    }

    fn union(&mut self, cs: Vec<Self::C>) -> Self::C {
        let mut cs = cs.into_iter();
        let first = cs.next().expect("check rejects an empty Union");
        cs.fold(first, |acc, c| acc.concat(c))
    }

    fn negate(&mut self, c: Self::C) -> Self::C {
        c.negate()
    }

    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>]) -> Result<Self::C, EngineError> {
        if cs.len() != 2 {
            return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join arity != 2")));
        }
        let side = |input: u8| -> Vec<ColId> {
            eq.iter()
                .map(|class| class.iter().find(|(i, _)| *i == input).map(|(_, c)| *c).expect("check: class spans both inputs"))
                .collect()
        };
        let (lk, rk) = (side(0), side(1));
        let mut cs = cs.into_iter();
        let (l, r) = (cs.next().unwrap(), cs.next().unwrap());
        let l = l.map(move |row| (cols(&row, &lk), row));
        let r = r.map(move |row| (cols(&row, &rk), row));
        Ok(l.join(r).map(|(_, (mut a, b))| {
            a.extend(b);
            a
        }))
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
            (false, true) => accumulable_values(c, key, linear),
            (true, false) => accumulable_values(c.clone(), key.clone(), vec![Agg::Count])
                .join(hierarchical_extrema(c, key, extrema, extrema_types, self.interner.clone(), self.reduce_reads.clone()))
                .map(|(group, (_, values))| (group, values)),
            (false, false) => accumulable_values(c.clone(), key.clone(), linear)
                .join(hierarchical_extrema(c, key, extrema, extrema_types, self.interner.clone(), self.reduce_reads.clone()))
                .map(move |(group, (linear, extrema))| {
                    let (mut li, mut ei) = (linear.into_iter(), extrema.into_iter());
                    let values = aggs.iter().map(|a| match a {
                        Agg::Count | Agg::Sum(_) => li.next().unwrap(),
                        Agg::Min(_) | Agg::Max(_) => ei.next().unwrap(),
                    }).collect();
                    (group, values)
                }),
            (true, true) => accumulable_values(c, key, linear),
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
fn accumulable_values<'s, T: Nest>(c: Coll<'s, T>, key: Vec<ColId>, aggs: Vec<Agg>) -> VecCollection<'s, T, (Row, Row), W> {
    let sums: Vec<ColId> = aggs.iter().filter_map(|a| if let Agg::Sum(c) = a { Some(*c) } else { None }).collect();
    c.explode(move |row: Row| {
        let mut acc = vec![1 as W];
        acc.extend(sums.iter().map(|c| row[*c as usize]));
        Some(((cols(&row, &key), ()), acc))
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
        Agg::Min(_) => live.clone().map(|row| row[i]).min_by(|a, b| cmp_cell(types[i], *a, *b, interner)).unwrap(),
        Agg::Max(_) => live.clone().map(|row| row[i]).max_by(|a, b| cmp_cell(types[i], *a, *b, interner)).unwrap(),
        _ => unreachable!(),
    }).collect();
    output.push((values, 1));
}

/// `order` first, then the whole row ascending, so ties resolve the same way in every engine.
fn cmp_cell(ty: Ty, a: Cell, b: Cell, interner: &Interner) -> Ordering {
    if ty == Ty::Id { interner.compare(a, b) } else { a.cmp(&b) }
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
    InternText(String, mpsc::Sender<Cell>),
    Text(Cell, mpsc::Sender<Option<String>>),
    Stop,
}

pub struct Dd {
    tx: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    counters: Counters,
}

/// Runs on the worker before the dataflow is built; tests register timely/differential loggers here.
pub type Hook = Box<dyn FnOnce(&mut timely::worker::Worker) + Send>;

impl Dd {
    pub fn install(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, false)
    }

    pub fn install_observed(program: &Program, hook: Hook) -> Result<Self, EngineError> {
        Self::start(program, Some(hook), false)
    }

    /// Every IR node's output collection gets an `inspect`; `settle_traced` returns what they saw.
    pub fn install_traced(program: &Program) -> Result<Self, EngineError> {
        Self::start(program, None, true)
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

    fn start(program: &Program, hook: Option<Hook>, traced: bool) -> Result<Self, EngineError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let program = program.clone();
        let thread = thread::spawn(move || worker(program, hook, traced, rx, ready_tx));
        let worker_gone = |_| EngineError::new(Stage::Install, None, ErrorKind::Worker("worker exited".into()));
        ready_rx.recv().map_err(worker_gone)??;
        Ok(Self { tx, thread: Some(thread), counters: Counters::default() })
    }
}

impl Engine for Dd {
    fn install(program: &Program, _host: &mut impl Host) -> Result<Self, EngineError> {
        Self::start(program, None, false)
    }

    fn settle(&mut self, frontier: Frontier, _host: &mut impl Host) -> Result<Delta, EngineError> {
        self.settle_traced(frontier).map(|(delta, _)| delta)
    }

    fn counters(&self) -> Counters { self.counters }

    fn snapshot(&self, rel: RelId, _host: &mut impl Host) -> Result<Vec<(Row, W)>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Snapshot(rel, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
    }

    fn intern_snapshot(&self, functor: RelId, _host: &mut impl Host) -> Result<Vec<(Row, W)>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternSnapshot(functor, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
    }
    fn intern_text(&mut self, value: &str, _host: &mut impl Host) -> Result<Cell, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::InternText(value.to_owned(), reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Settle, e))
    }
    fn text(&self, id: Cell, _host: &mut impl Host) -> Result<Option<String>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Text(id, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))
    }
}

fn worker_error(stage: Stage, e: impl std::fmt::Display) -> EngineError {
    EngineError::new(stage, None, ErrorKind::Worker(e.to_string()))
}

impl Drop for Dd {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Built {
    inputs: BTreeMap<RelId, InputSession<Time, Row, W>>,
    constructor_inputs: BTreeMap<RelId, InputSession<Time, Row, W>>,
    guards: BTreeMap<RelId, Trace>,
    outputs: BTreeMap<RelId, Trace>,
}

fn worker(program: Program, hook: Option<Hook>, traced: bool, rx: mpsc::Receiver<Command>, ready: mpsc::Sender<Result<(), EngineError>>) {
    let rx = std::sync::Mutex::new(rx);
    let hook = std::sync::Mutex::new(hook);
    timely::execute_directly(move |worker| {
        if let Some(hook) = hook.lock().unwrap().take() {
            hook(worker);
        }
        let reduce_reads = worker.log_register().and_then(|registry| registry.get::<ReduceReadEventBuilder>("lab/reduce_reads"));
        let mut probe = ProbeHandle::new();
        let captured: Rc<RefCell<Vec<(RelId, Row, Time, W)>>> = Rc::default();
        let taps: Option<Rc<RefCell<Vec<DdTap>>>> = Some(Rc::default());
        let interner = Rc::new(RefCell::new(Interner::default()));
        let texts = {
            let mut dict = interner.borrow_mut();
            if program.uses_strings() { dict.mint_text(""); }
            program.texts.iter().map(|text| dict.mint_text(text)).collect::<Vec<_>>()
        };
        let constructors: BTreeMap<RelId, (String, Vec<Ty>)> = program.rels.iter()
            .filter(|r| r.kind == RelKind::Constructor)
            .map(|r| (r.id, (r.name.clone(), r.cols.iter().skip(1).copied().collect())))
            .collect();
        let built = worker.dataflow::<Time, _, _>(|scope| -> Result<Built, EngineError> {
            let mut inputs = BTreeMap::new();
            let mut constructor_inputs = BTreeMap::new();
            let mut guards = BTreeMap::new();
            let mut sources = BTreeMap::new();
            for rel in program.rels.iter().filter(|r| r.kind == RelKind::Source) {
                let (input, c) = scope.new_collection::<Row, W>();
                let guard = c.clone().arrange_by_self();
                guard.stream.clone().probe_with(&mut probe);
                inputs.insert(rel.id, input);
                guards.insert(rel.id, guard.trace);
                sources.insert(rel.id, c);
            }
            for rel in program.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
                let (input, c) = scope.new_collection::<Row, W>();
                c.clone().probe_with(&mut probe);
                constructor_inputs.insert(rel.id, input);
                sources.insert(rel.id, c);
            }
            let mut rel = DdRel { scope, rec: None, sources, outputs: Vec::new(), taps: taps.clone(), reduce_reads: reduce_reads.clone(), constructors: constructors.clone(), interner: interner.clone(), texts: texts.clone() };
            lower(&program, &mut rel)?;
            let mut outputs = BTreeMap::new();
            for (id, c) in rel.outputs {
                let arranged = c.arrange_by_self();
                let sink = Rc::clone(&captured);
                arranged
                    .clone()
                    .as_collection(|row: &Row, _| row.clone())
                    .inspect(move |(row, t, w)| sink.borrow_mut().push((id, row.clone(), *t, *w)))
                    .probe_with(&mut probe);
                outputs.insert(id, arranged.trace);
            }
            Ok(Built { inputs, constructor_inputs, guards, outputs })
        });
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
        let mut epoch: Time = 0;
        let mut frontier_tick = 0;
        loop {
            let Ok(command) = rx.lock().unwrap().recv() else { break };
            match command {
                Command::Settle(frontier, reply) => {
                    let interned_before = interner.borrow().len();
                    let accepted = match guard(&program, &mut built.guards, epoch, &frontier) {
                        Ok(accepted) => accepted,
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            continue;
                        }
                    };
                    for change in accepted {
                        built.inputs.get_mut(&change.rel).unwrap().update(change.row, change.w);
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
                    worker.step_while(|| probe.less_than(&epoch));
                    loop {
                        let pending = interner.borrow_mut().drain_pending();
                        if pending.is_empty() { break; }
                        for (name, row) in pending {
                            let id = constructors.iter().find(|(_, (functor, _))| functor == &name).map(|(id, _)| id).copied();
                            if let Some(id) = id {
                                built.constructor_inputs.get_mut(&id).unwrap().update(row, 1);
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
                        worker.step_while(|| probe.less_than(&epoch));
                    }
                    if let Some(logger) = &reduce_reads { logger.flush(); }
                    for trace in built.guards.values_mut().chain(built.outputs.values_mut()) {
                        trace.set_logical_compaction(AntichainRef::new(&[epoch]));
                        trace.set_physical_compaction(AntichainRef::new(&[epoch]));
                    }
                    let mut net: BTreeMap<(RelId, Row), W> = BTreeMap::new();
                    for (rel, row, _, w) in captured.borrow_mut().drain(..) {
                        *net.entry((rel, row)).or_default() += w;
                    }
                    let changes: Vec<_> = net.into_iter()
                        .filter(|(_, w)| *w != 0)
                        .map(|((rel, row), w)| (rel, row, w))
                        .collect();
                    let mut seen: BTreeMap<(NodeId, u64, Option<RelId>, Option<u64>, Row), W> = BTreeMap::new();
                    for tap in taps.iter().flat_map(|t| t.borrow_mut().drain(..).collect::<Vec<_>>()) {
                        *seen.entry((tap.node, frontier_tick, tap.rec, tap.round, tap.row)).or_default() += tap.w;
                    }
                    let seen: Vec<DdTap> = seen
                        .into_iter()
                        .filter(|(_, w)| *w != 0)
                        .map(|((node, tick, rec, round, row), w)| DdTap { node, tick, rec, round, row, w })
                        .collect();
                    let mut counters = Counters::measured();
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
                            Some(Op::Mint { .. } | Op::StrCons { .. }) => &mut counters.delta_rows.mint,
                            _ => continue,
                        };
                        *field.as_mut().unwrap() += 1;
                    }
                    counters.rounds = Some(rounds.into_iter().filter(|(_, round)| *round > 0).count() as u64);
                    let _ = reply.send(Ok((Delta { tick: frontier_tick, changes }, if traced { seen } else { Vec::new() }, counters)));
                    frontier_tick += 1;
                }
                Command::Snapshot(rel, reply) => {
                    let answer = built
                        .outputs
                        .get_mut(&rel)
                        .map(read_trace)
                        .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)));
                    let _ = reply.send(answer);
                }
                Command::InternSnapshot(rel, reply) => {
                    let answer = constructors.get(&rel)
                        .map(|(name, _)| interner.borrow().snapshot(name))
                        .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)));
                    let _ = reply.send(answer);
                }
                Command::InternText(value, reply) => { let _ = reply.send(interner.borrow_mut().mint_text(&value)); }
                Command::Text(id, reply) => { let _ = reply.send(interner.borrow().text(id).map(str::to_owned)); }
                Command::Stop => break,
            }
        }
    });
}

fn count(trace: &mut Trace, row: &Row) -> W {
    let (mut cursor, storage) = trace.cursor();
    cursor.seek_key(&storage, row);
    let mut n: W = 0;
    if cursor.get_key(&storage) == Some(row) {
        cursor.map_times(&storage, |_, w| n += *w);
    }
    n
}

fn read_trace(trace: &mut Trace) -> Vec<(Row, W)> {
    let (mut cursor, storage) = trace.cursor();
    let mut rows = Vec::new();
    while let Some(row) = cursor.get_key(&storage) {
        let row = row.clone();
        let mut n: W = 0;
        cursor.map_times(&storage, |_, w| n += *w);
        if n != 0 {
            rows.push((row, n));
        }
        cursor.step_key(&storage);
    }
    rows
}

/// Source relations are sets. Insert of a present row rejects the frontier; delete of an
/// absent row is a no-op, like SQL `DELETE` matching nothing, and logs a warning.
fn guard(
    program: &Program,
    guards: &mut BTreeMap<RelId, Trace>,
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
        let before = *pending.get(&key).unwrap_or(&0) + count(guards.get_mut(&rel.id).unwrap(), &change.row);
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
    Ok(accepted)
}
