//! Differential-dataflow arm, the in-process port of
//! `bench/shared/34_circuit_dd.rs` (circuit graphs, frontier check, observed
//! input bag) and `37_semantic_graphs.rs` (semantic graphs).
//! `timely::execute_directly` owns a worker thread; per-state jobs cross via
//! channels so the Arm trait keeps its setup/apply/teardown shape.

use super::{Arm, Measure, Setup};
use crate::fixture::{CIRCUITS, Fixture, State, Write, WriteRow};
use crate::oracle::{digest, output_text, Cell};
use anyhow::{anyhow, bail, Result};
use differential_dataflow::input::Input;
use differential_dataflow::operators::{CountTotal, Iterate};
use differential_dataflow::VecCollection;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;
use timely::dataflow::operators::probe::Handle;
use timely::worker::Worker;

type Session = differential_dataflow::input::InputSession<u64, [i64; 3], isize>;

struct Job {
    name: String,
    index: usize,
    writes: Vec<Write>,
    expected_rows: Vec<Vec<i64>>,
    expected_inputs: [Vec<[i64; 3]>; 3],
    input_hash: String,
}

impl Job {
    fn from_state(state: &State, index: usize) -> Result<Job> {
        let int = |cell: &Cell| -> Result<i64> {
            cell.as_int().ok_or_else(|| anyhow!("dd arm requires the integer domain"))
        };
        let to_rows = |rows: &[WriteRow]| -> Result<Vec<[i64; 3]>> {
            rows.iter().map(|r| Ok([r.0, int(&r.1)?, int(&r.2)?])).collect()
        };
        Ok(Job {
            name: state.name.clone(),
            index,
            writes: state.writes.clone(),
            expected_rows: state
                .expected
                .rows
                .iter()
                .map(|row| row.iter().map(&int).collect())
                .collect::<Result<Vec<_>>>()?,
            expected_inputs: [
                to_rows(&state.inputs.a)?,
                to_rows(&state.inputs.b)?,
                to_rows(&state.inputs.c)?,
            ],
            input_hash: state.input_hash.clone(),
        })
    }
}

pub struct Dd {
    jobs: Option<Sender<Option<Job>>>,
    results: Option<Receiver<Result<Measure>>>,
    join: Option<std::thread::JoinHandle<()>>,
    applied: usize,
}

impl Dd {
    pub fn new() -> Self {
        Self { jobs: None, results: None, join: None, applied: 0 }
    }
}

impl Default for Dd {
    fn default() -> Self {
        Self::new()
    }
}

impl Arm for Dd {
    fn name(&self) -> &'static str {
        "dd"
    }

    fn setup(&mut self, fixture: &Fixture) -> Result<Setup> {
        let family = fixture.circuit.clone();
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<Option<Job>>();
        let (results_tx, results_rx) = std::sync::mpsc::channel::<Result<Measure>>();
        // `execute_directly` needs a Sync closure; mpsc endpoints are not Sync.
        let panic_tx = results_tx.clone();
        let jobs_rx = Mutex::new(jobs_rx);
        let results_tx = Mutex::new(results_tx);
        let join = std::thread::spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                timely::execute_directly(move |worker: &mut Worker| {
                    run_case(worker, &family, jobs_rx, results_tx);
                });
            }));
            if ran.is_err() {
                let _ = panic_tx.send(Err(anyhow!("dd worker panicked")));
            }
        });
        self.jobs = Some(jobs_tx);
        self.results = Some(results_rx);
        self.join = Some(join);
        self.applied = 0;
        Ok(Setup::Ready)
    }

    fn apply(&mut self, state: &State) -> Result<Measure> {
        let job = Job::from_state(state, self.applied)?;
        self.jobs
            .as_ref()
            .ok_or_else(|| anyhow!("dd setup missing"))?
            .send(Some(job))?;
        self.applied += 1;
        self.results
            .as_ref()
            .ok_or_else(|| anyhow!("dd setup missing"))?
            .recv()?
    }

    fn teardown(&mut self) -> Result<()> {
        self.jobs = None; // closes the channel; the worker drains and exits
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| anyhow!("dd worker panicked"))?;
        }
        Ok(())
    }
}

/// `canonical` from the DD hosts: one `prefix<TAB>row` line per row.
fn canonical(prefix: &str, rows: &[Vec<i64>]) -> String {
    rows.iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(ToString::to_string).collect();
            format!("{prefix}\t{}\n", cells.join("\t"))
        })
        .collect()
}

/// Port of the circuit graph dispatch in `34_circuit_dd.rs`.
fn circuit_graph<'s>(
    family: &str,
    ak: VecCollection<'s, u64, (i64, i64)>,
    bk: VecCollection<'s, u64, (i64, i64)>,
    ck: VecCollection<'s, u64, (i64, i64)>,
) -> VecCollection<'s, u64, Vec<i64>> {
    match family {
        "pipeline" => ak.filter(|(_, v)| *v >= 0).map(|(k, v)| vec![k, v * 2]),
        "fanout_fanin" => ak
            .clone()
            .filter(|(_, v)| *v >= 0)
            .concat(ak.filter(|(_, v)| v % 2 == 0))
            .map(|(k, v)| vec![k, v]),
        "distinct" => ak.distinct().map(|(k, v)| vec![k, v]),
        "join" => ak.join(bk).map(|(k, (x, y))| vec![k, x * y]),
        "self_join" => ak
            .clone()
            .map(|(k, v)| (v, k))
            .join(ak)
            .map(|(_, (k, v))| vec![k, v]),
        "chain" => ak
            .map(|(k, v)| (v, k))
            .join(bk)
            .map(|(_, (k, v))| (v, k))
            .join(ck)
            .map(|(_, (k, v))| vec![k, v]),
        "diamond" => ak
            .map(|(k, v)| (v, k))
            .join(bk.concat(ck))
            .map(|(_, (k, v))| vec![k, v]),
        "semijoin" => ak
            .semijoin(bk.map(|(k, _)| k).distinct())
            .map(|(k, v)| vec![k, v]),
        "antijoin" => ak
            .antijoin(bk.map(|(k, _)| k).distinct())
            .map(|(k, v)| vec![k, v]),
        "aggregate_churn" => ak
            .join(bk)
            .explode(|(k, (x, y))| Some((k, (1isize, isize::try_from(x * y).unwrap()))))
            .count_total()
            .map(|(k, (n, s))| vec![k, n as i64, s as i64]),
        "reach_cycle" => {
            let edges = ak;
            let roots = bk.map(|(k, _)| k).distinct();
            let edges_c = edges.clone();
            let roots_c = roots.clone();
            roots
                .iterate(move |scope, inner| {
                    edges_c
                        .enter(scope)
                        .semijoin(inner)
                        .map(|(_, child)| child)
                        .concat(roots_c.enter(scope))
                        .distinct()
                })
                .map(|k| vec![k])
        }
        other => unreachable!("validated circuit family {other}"),
    }
}

/// Port of the semantic graph dispatch in `37_semantic_graphs.rs`.
fn semantic_graph<'s>(
    family: &str,
    a: VecCollection<'s, u64, [i64; 3]>,
    b: VecCollection<'s, u64, [i64; 3]>,
) -> VecCollection<'s, u64, Vec<i64>> {
    let ak = a.clone().map(|[_, k, v]| (k, v));
    let bk = b.clone().map(|[_, k, v]| (k, v));
    match family {
        "minmax" => ak
            .reduce(|_, input, output| {
                let n: isize = input.iter().map(|(_, weight)| *weight).sum();
                output.push((
                    (n as i64, *input.first().unwrap().0, *input.last().unwrap().0),
                    1,
                ));
            })
            .map(|(k, (n, min, max))| vec![k, n, min, max]),
        "count_distinct" => ak
            .distinct()
            .map(|(k, _)| k)
            .count_total()
            .map(|(k, n)| vec![k, n as i64]),
        "union_set" => ak.concat(bk).distinct().map(|(k, v)| vec![k, v]),
        "except_set" => ak
            .clone()
            .distinct()
            .map(|kv| (kv, ()))
            .antijoin(bk.distinct())
            .map(|((k, v), _)| vec![k, v]),
        "intersect_set" => ak
            .clone()
            .distinct()
            .map(|kv| (kv, ()))
            .semijoin(bk.distinct())
            .map(|((k, v), _)| vec![k, v]),
        "topk" => a
            .map(|[id, k, v]| ((), (-v, id, k)))
            .reduce(|_, input, output| {
                for (row, weight) in input.iter().take(3) {
                    assert_eq!(*weight, 1);
                    output.push((**row, 1));
                }
            })
            .map(|(_, (negative_v, _, k))| vec![k, -negative_v]),
        "window_rank" => a
            .map(|[id, k, v]| (k, (v, id)))
            .reduce(|_, input, output| {
                for (index, (row, weight)) in input.iter().enumerate() {
                    assert_eq!(*weight, 1);
                    output.push(((row.0, row.1, index as i64 + 1), 1));
                }
            })
            .map(|(k, (v, _, rank))| vec![k, v, rank]),
        "subquery" | "cte" => ak.filter(|(_, v)| *v >= 0).map(|(k, v)| vec![k, v]),
        other => unreachable!("validated semantic family {other}"),
    }
}

fn run_case(
    worker: &mut Worker,
    family: &str,
    jobs: Mutex<Receiver<Option<Job>>>,
    results: Mutex<Sender<Result<Measure>>>,
) {
    let output = Arc::new(Mutex::new(BTreeMap::<Vec<i64>, isize>::new()));
    let observed = Arc::new(Mutex::new(BTreeMap::<(usize, [i64; 3]), isize>::new()));
    let probe = Handle::new();
    let mut inputs: [Session; 3] = worker.dataflow::<u64, _, _>(|scope| {
        let (ai, a) = scope.new_collection::<[i64; 3], isize>();
        let (bi, b) = scope.new_collection::<[i64; 3], isize>();
        let (ci, c) = scope.new_collection::<[i64; 3], isize>();
        for (index, collection) in [a.clone(), b.clone(), c.clone()].into_iter().enumerate() {
            let observed = Arc::clone(&observed);
            collection
                .inspect(move |(r, _, d)| {
                    let mut observed = observed.lock();
                    let key = (index, *r);
                    *observed.entry(key).or_default() += d;
                    if observed[&key] == 0 {
                        observed.remove(&key);
                    }
                })
                .probe_with(&probe);
        }
        let ak = a.clone().map(|[_, k, v]| (k, v));
        let bk = b.clone().map(|[_, k, v]| (k, v));
        let ck = c.clone().map(|[_, k, v]| (k, v));
        let result = if CIRCUITS.contains(&family) {
            circuit_graph(family, ak, bk, ck)
        } else {
            semantic_graph(family, a.clone(), b.clone())
        };
        let output = Arc::clone(&output);
        result
            .consolidate()
            .inspect(move |(r, _, d)| {
                let mut output = output.lock();
                *output.entry(r.clone()).or_default() += d;
                if output[r] == 0 {
                    output.remove(r);
                }
            })
            .probe_with(&probe);
        [ai, bi, ci]
    });

    // Separate finite frontier check before timed mutations, ported from
    // `34_circuit_dd.rs`: a retained third input frontier must prevent the
    // combined probe completing.
    for input in &mut inputs[..2] {
        input.advance_to(1);
        input.flush();
    }
    for _ in 0..128 {
        worker.step();
    }
    if !probe.less_than(&1) {
        let _ = results.lock().send(Err(anyhow!("frontier check failed: c frontier did not hold probe")));
        return;
    }
    inputs[2].advance_to(1);
    inputs[2].flush();
    while probe.less_than(&1) {
        worker.step();
    }

    let mut keyed: [BTreeMap<i64, [i64; 3]>; 3] = Default::default();
    while let Ok(Some(job)) = jobs.lock().recv() {
        let result = apply_job(&job, worker, &mut inputs, &mut keyed, &probe, &output, &observed);
        if results.lock().send(result).is_err() {
            break;
        }
    }
}

fn apply_job(
    job: &Job,
    worker: &mut Worker,
    inputs: &mut [Session; 3],
    keyed: &mut [BTreeMap<i64, [i64; 3]>; 3],
    probe: &Handle<u64>,
    output: &Mutex<BTreeMap<Vec<i64>, isize>>,
    observed: &Mutex<BTreeMap<(usize, [i64; 3]), isize>>,
) -> Result<Measure> {
    let start = Instant::now();
    for write in &job.writes {
        let table = match write.table {
            "a" => 0,
            "b" => 1,
            _ => 2,
        };
        if let Some(old) = keyed[table].remove(&write.id) {
            inputs[table].update(old, -1);
        }
        if let Some(row) = &write.row {
            let triple = [row.0, row.1.as_int().unwrap_or(0), row.2.as_int().unwrap_or(0)];
            keyed[table].insert(write.id, triple);
            inputs[table].update(triple, 1);
        }
    }
    let epoch = job.index as u64 + 2;
    for input in inputs.iter_mut() {
        input.advance_to(epoch);
        input.flush();
    }
    while probe.less_than(&epoch) {
        worker.step();
    }
    let update = start.elapsed();
    let start = Instant::now();
    let mut rows = Vec::new();
    for (row, delta) in output.lock().iter() {
        if *delta <= 0 {
            bail!("{}: non-positive output multiplicity", job.name);
        }
        for _ in 0..*delta {
            rows.push(row.clone());
        }
    }
    let compute = start.elapsed();
    if rows != job.expected_rows {
        bail!("{}: output mismatch", job.name);
    }
    // Source rows, port of the input check in `34_circuit_dd.rs`.
    let mut input_text = String::new();
    for (table, name) in ["a", "b", "c"].iter().enumerate() {
        let mut actual: Vec<Vec<i64>> = observed
            .lock()
            .iter()
            .filter(|((t, _), _)| *t == table)
            .map(|((_, row), delta)| {
                if *delta != 1 {
                    bail!("{}: non-unit input multiplicity", job.name);
                }
                Ok(row.to_vec())
            })
            .collect::<Result<Vec<_>>>()?;
        actual.sort();
        let mut expected: Vec<[i64; 3]> = job.expected_inputs[table].to_vec();
        expected.sort();
        if actual != expected {
            bail!("{}: source rows mismatch on table {name}", job.name);
        }
        input_text.push_str(&canonical(&name.to_uppercase(), &actual));
    }
    if digest(&input_text) != job.input_hash {
        bail!("{}: input hash mismatch", job.name);
    }
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|row| row.iter().map(|value| Cell::Int(*value)).collect())
        .collect();
    let checksum = digest(&output_text(&cells));
    Ok(Measure { wall: update + compute, checksum })
}
