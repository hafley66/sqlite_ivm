//! Differential-dataflow arm for the scale sweep.
//!
//! The fixture-driven `arms::dd` arm replays named states; the scale sweep
//! streams raw mutations instead, so this runs the same timely worker shape
//! with a scale protocol: seed the three input sessions, apply a batch of
//! writes, read the output bag. `timely::execute_directly` owns a worker
//! thread and the protocol crosses via channels.

use crate::fixture::Write;
use differential_dataflow::input::Input;
use differential_dataflow::operators::{CountTotal, Iterate};
use differential_dataflow::VecCollection;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use timely::dataflow::operators::probe::Handle;
use timely::worker::Worker;

type Session = differential_dataflow::input::InputSession<u64, [i64; 3], isize>;

/// Scale circuits that have a dataflow graph.
pub const FAMILIES: [&str; 6] = ["chain", "join", "distinct", "group", "window", "reach"];

enum Job {
    Seed(Box<[Vec<[i64; 3]>; 3]>),
    Apply { writes: Vec<Write>, one_txn: bool },
    Read,
}

enum Reply {
    Ready,
    Applied(Duration),
    Rows(Vec<Vec<i64>>),
    Failed(String),
}

pub struct DdScale {
    jobs: Option<Sender<Job>>,
    replies: Option<Receiver<Reply>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl DdScale {
    pub fn start(family: &str) -> anyhow::Result<Self> {
        if !FAMILIES.contains(&family) {
            anyhow::bail!("no dd graph for circuit {family}");
        }
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<Job>();
        let (replies_tx, replies_rx) = std::sync::mpsc::channel::<Reply>();
        let panic_tx = replies_tx.clone();
        let jobs_rx = Mutex::new(jobs_rx);
        let replies_tx = Mutex::new(replies_tx);
        let family = family.to_string();
        let join = std::thread::spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                timely::execute_directly(move |worker: &mut Worker| {
                    run(worker, &family, jobs_rx, replies_tx);
                });
            }));
            if ran.is_err() {
                let _ = panic_tx.send(Reply::Failed("dd worker panicked".into()));
            }
        });
        Ok(Self { jobs: Some(jobs_tx), replies: Some(replies_rx), join: Some(join) })
    }

    pub fn seed(&self, tables: [Vec<[i64; 3]>; 3]) -> anyhow::Result<()> {
        self.send(Job::Seed(Box::new(tables)))?;
        match self.recv()? {
            Reply::Ready => Ok(()),
            Reply::Failed(why) => anyhow::bail!(why),
            _ => anyhow::bail!("dd seed got an unexpected reply"),
        }
    }

    pub fn apply(&self, writes: &[Write], one_txn: bool) -> anyhow::Result<Duration> {
        self.send(Job::Apply { writes: writes.to_vec(), one_txn })?;
        match self.recv()? {
            Reply::Applied(wall) => Ok(wall),
            Reply::Failed(why) => anyhow::bail!(why),
            _ => anyhow::bail!("dd apply got an unexpected reply"),
        }
    }

    pub fn read(&self) -> anyhow::Result<Vec<Vec<i64>>> {
        self.send(Job::Read)?;
        match self.recv()? {
            Reply::Rows(rows) => Ok(rows),
            Reply::Failed(why) => anyhow::bail!(why),
            _ => anyhow::bail!("dd read got an unexpected reply"),
        }
    }

    pub fn teardown(&mut self) -> anyhow::Result<()> {
        self.jobs = None; // closes the channel; the worker drains and exits
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| anyhow::anyhow!("dd worker panicked"))?;
        }
        Ok(())
    }

    fn send(&self, job: Job) -> anyhow::Result<()> {
        self.jobs
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dd arm is torn down"))?
            .send(job)?;
        Ok(())
    }

    fn recv(&self) -> anyhow::Result<Reply> {
        Ok(self
            .replies
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dd arm is torn down"))?
            .recv()?)
    }
}

fn run(
    worker: &mut Worker,
    family: &str,
    jobs: Mutex<Receiver<Job>>,
    replies: Mutex<Sender<Reply>>,
) {
    let output = Arc::new(Mutex::new(BTreeMap::<Vec<i64>, isize>::new()));
    let probe = Handle::new();
    let mut inputs: [Session; 3] = worker.dataflow::<u64, _, _>(|scope| {
        let (ai, a) = scope.new_collection::<[i64; 3], isize>();
        let (bi, b) = scope.new_collection::<[i64; 3], isize>();
        let (ci, c) = scope.new_collection::<[i64; 3], isize>();
        let output = Arc::clone(&output);
        graph(family, a, b, c)
            .consolidate()
            .inspect(move |(row, _, delta)| {
                let mut output = output.lock();
                *output.entry(row.clone()).or_default() += delta;
                if output[row] == 0 {
                    output.remove(row);
                }
            })
            .probe_with(&probe);
        [ai, bi, ci]
    });

    let mut epoch = 0u64;
    let mut keyed: [BTreeMap<i64, [i64; 3]>; 3] = Default::default();
    let advance = |inputs: &mut [Session; 3], worker: &mut Worker, epoch: u64| {
        for input in inputs.iter_mut() {
            input.advance_to(epoch);
            input.flush();
        }
        while probe.less_than(&epoch) {
            worker.step();
        }
    };
    while let Ok(job) = jobs.lock().recv() {
        let reply = match job {
            Job::Seed(tables) => {
                for (table, rows) in tables.into_iter().enumerate() {
                    for row in rows {
                        keyed[table].insert(row[0], row);
                        inputs[table].update(row, 1);
                    }
                }
                epoch += 1;
                advance(&mut inputs, worker, epoch);
                Reply::Ready
            }
            Job::Apply { writes, one_txn } => {
                let started = Instant::now();
                let mut next = epoch;
                for write in &writes {
                    let table = match write.table {
                        "a" => 0,
                        "b" => 1,
                        _ => 2,
                    };
                    if let Some(old) = keyed[table].remove(&write.id) {
                        inputs[table].update(old, -1);
                    }
                    if let Some(row) = &write.row {
                        let triple =
                            [row.0, row.1.as_int().unwrap_or(0), row.2.as_int().unwrap_or(0)];
                        keyed[table].insert(write.id, triple);
                        inputs[table].update(triple, 1);
                    }
                    if !one_txn {
                        next += 1;
                        advance(&mut inputs, worker, next);
                    }
                }
                if one_txn {
                    next += 1;
                    advance(&mut inputs, worker, next);
                }
                epoch = next;
                Reply::Applied(started.elapsed())
            }
            Job::Read => {
                let mut rows = Vec::new();
                let mut failure = None;
                for (row, delta) in output.lock().iter() {
                    if *delta < 0 {
                        failure = Some("negative output multiplicity".to_string());
                        break;
                    }
                    for _ in 0..*delta {
                        rows.push(row.clone());
                    }
                }
                match failure {
                    Some(why) => Reply::Failed(why),
                    None => Reply::Rows(rows),
                }
            }
        };
        if replies.lock().send(reply).is_err() {
            break;
        }
    }
}

/// The scale-circuit dataflow graphs, one arm per circuit.
fn graph<'s>(
    family: &str,
    a: VecCollection<'s, u64, [i64; 3]>,
    b: VecCollection<'s, u64, [i64; 3]>,
    c: VecCollection<'s, u64, [i64; 3]>,
) -> VecCollection<'s, u64, Vec<i64>> {
    let ak = a.clone().map(|[_, k, v]| (k, v));
    let bk = b.clone().map(|[_, k, v]| (k, v));
    let ck = c.clone().map(|[_, k, v]| (k, v));
    match family {
        "chain" => ak
            .map(|(k, v)| (v, k))
            .join(bk)
            .map(|(_, (k, v))| (v, k))
            .join(ck)
            .map(|(_, (k, v))| vec![k, v]),
        "join" => ak.join(bk).map(|(k, (x, y))| vec![k, x * y]),
        "distinct" => ak.map(|(_, v)| vec![v]).distinct(),
        "group" => ak
            .explode(|(k, v)| Some((k, (1isize, v as isize))))
            .count_total()
            .map(|(k, (n, s))| vec![k, n as i64, s as i64]),
        "window" => a
            .map(|[id, k, v]| (k, (v, id)))
            .reduce(|_, input, output| {
                for (index, (row, weight)) in input.iter().enumerate() {
                    assert_eq!(*weight, 1);
                    output.push(((row.0, row.1, index as i64 + 1), 1));
                }
            })
            .map(|(k, (v, _, rank))| vec![k, v, rank]),
        "reach" => {
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
        other => unreachable!("validated scale family {other}"),
    }
}