//! differential-dataflow engine: `lower` runs once inside `worker.dataflow`; each `settle` is one epoch.

use crate::ir::*;
use crate::rel::*;
use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::arrange::TraceAgent;
use differential_dataflow::trace::cursor::Cursor;
use differential_dataflow::trace::implementations::KeySpine;
use differential_dataflow::trace::TraceReader;
use differential_dataflow::VecCollection;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::dataflow::operators::Probe;
use timely::progress::frontier::AntichainRef;

type Time = u64;
type Coll<'s> = VecCollection<'s, Time, Row, W>;
type Trace = TraceAgent<KeySpine<Row, Time, W>>;

pub struct DdRel<'s> {
    sources: BTreeMap<RelId, Coll<'s>>,
    outputs: Vec<(RelId, Coll<'s>)>,
}

fn cols(row: &[Cell], cols: &[ColId]) -> Row {
    cols.iter().map(|c| row[*c as usize]).collect()
}

impl<'s> Rel for DdRel<'s> {
    type C = Coll<'s>;

    fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError> {
        self.sources
            .get(&rel)
            .cloned()
            .ok_or_else(|| EngineError::new(Stage::Install, Some(rel), ErrorKind::UnknownRel(rel)))
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C {
        let (filter, map, project) = (filter.to_vec(), map.to_vec(), project.to_vec());
        c.flat_map(move |mut row: Row| {
            if !filter.iter().all(|e| eval(e, &row) != 0) {
                return None;
            }
            for e in &map {
                let v = eval(e, &row);
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

    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg]) -> Self::C {
        let (key, aggs) = (key.to_vec(), aggs.to_vec());
        c.map(move |row| (cols(&row, &key), row))
            .reduce(move |_k, input: &[(&Row, W)], output: &mut Vec<(Row, W)>| {
                let count: W = input.iter().map(|(_, w)| *w).sum();
                if count <= 0 {
                    return;
                }
                let live = || input.iter().filter(|(_, w)| *w > 0).map(|(row, _)| *row);
                let values = aggs
                    .iter()
                    .map(|agg| match agg {
                        Agg::Count => count,
                        Agg::Sum(c) => input.iter().map(|(row, w)| row[*c as usize] * w).sum(),
                        Agg::Min(c) => live().map(|row| row[*c as usize]).min().unwrap(),
                        Agg::Max(c) => live().map(|row| row[*c as usize]).max().unwrap(),
                    })
                    .collect();
                output.push((values, 1));
            })
            .map(|(mut k, values)| {
                k.extend(values);
                k
            })
    }

    fn threshold(&mut self, c: Self::C) -> Self::C {
        c.threshold(|_, w: &W| if *w > 0 { 1 as W } else { 0 })
    }

    fn output(&mut self, rel: RelId, c: Self::C) {
        self.outputs.push((rel, c));
    }
}

enum Command {
    Settle(Frontier, mpsc::Sender<Result<Delta, EngineError>>),
    Snapshot(RelId, mpsc::Sender<Result<Vec<(Row, W)>, EngineError>>),
    Stop,
}

pub struct Dd {
    tx: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Dd {
    pub fn install(program: &Program) -> Result<Self, EngineError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let program = program.clone();
        let thread = thread::spawn(move || worker(program, rx, ready_tx));
        let worker_gone = |_| EngineError::new(Stage::Install, None, ErrorKind::Worker("worker exited".into()));
        ready_rx.recv().map_err(worker_gone)??;
        Ok(Self { tx, thread: Some(thread) })
    }

    pub fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Settle(frontier, reply)).map_err(|e| worker_error(Stage::Settle, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Settle, e))?
    }

    pub fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let (reply, answer) = mpsc::channel();
        self.tx.send(Command::Snapshot(rel, reply)).map_err(|e| worker_error(Stage::Snapshot, e))?;
        answer.recv().map_err(|e| worker_error(Stage::Snapshot, e))?
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
    guards: BTreeMap<RelId, Trace>,
    outputs: BTreeMap<RelId, Trace>,
}

fn worker(program: Program, rx: mpsc::Receiver<Command>, ready: mpsc::Sender<Result<(), EngineError>>) {
    let rx = std::sync::Mutex::new(rx);
    timely::execute_directly(move |worker| {
        let mut probe = ProbeHandle::new();
        let captured: Rc<RefCell<Vec<(RelId, Row, Time, W)>>> = Rc::default();
        let built = worker.dataflow::<Time, _, _>(|scope| -> Result<Built, EngineError> {
            let mut inputs = BTreeMap::new();
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
            let mut rel = DdRel { sources, outputs: Vec::new() };
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
            Ok(Built { inputs, guards, outputs })
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
        loop {
            let Ok(command) = rx.lock().unwrap().recv() else { break };
            match command {
                Command::Settle(frontier, reply) => {
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
                    worker.step_while(|| probe.less_than(&epoch));
                    for trace in built.guards.values_mut().chain(built.outputs.values_mut()) {
                        trace.set_logical_compaction(AntichainRef::new(&[epoch]));
                        trace.set_physical_compaction(AntichainRef::new(&[epoch]));
                    }
                    let mut changes: Vec<(RelId, Row, W)> = captured
                        .borrow_mut()
                        .drain(..)
                        .map(|(rel, row, t, w)| {
                            debug_assert_eq!(t, epoch - 1);
                            (rel, row, w)
                        })
                        .collect();
                    changes.sort();
                    let _ = reply.send(Ok(Delta { tick: epoch - 1, changes }));
                }
                Command::Snapshot(rel, reply) => {
                    let answer = built
                        .outputs
                        .get_mut(&rel)
                        .map(read_trace)
                        .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)));
                    let _ = reply.send(answer);
                }
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
