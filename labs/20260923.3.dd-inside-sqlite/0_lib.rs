//! Differential Dataflow implementation of the two frontier packet queries.
//! The same worker is used by the Rust caller and the SQLite extension.

use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::CountTotal;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use timely::dataflow::operators::probe::Handle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Access,
    Group,
}

#[derive(Clone, Copy, Debug)]
pub struct Change {
    pub table: usize,
    pub id: i64,
    pub a: i64,
    pub b: i64,
    pub weight: isize,
}

pub type Delta = (Vec<i64>, isize);

enum Command {
    Apply(Vec<Change>, mpsc::Sender<Vec<Delta>>),
    Snapshot(mpsc::Sender<Vec<Vec<i64>>>),
    Stop,
}

pub struct Engine {
    sender: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(shape: Shape) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::channel();
        let thread = thread::spawn(move || run(shape, receiver, ready));
        initialized.recv().expect("DD worker initialized");
        Self {
            sender,
            thread: Some(thread),
        }
    }

    pub fn apply(&self, changes: Vec<Change>) -> Result<Vec<Delta>, String> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(Command::Apply(changes, reply))
            .map_err(|error| error.to_string())?;
        result.recv().map_err(|error| error.to_string())
    }

    pub fn snapshot(&self) -> Result<Vec<Vec<i64>>, String> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(Command::Snapshot(reply))
            .map_err(|error| error.to_string())?;
        result.recv().map_err(|error| error.to_string())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shape: Shape, receiver: mpsc::Receiver<Command>, ready: mpsc::Sender<()>) {
    let emitted = Arc::new(Mutex::new(BTreeMap::<Vec<i64>, isize>::new()));
    let receiver = Arc::new(Mutex::new(receiver));
    timely::execute_directly(move |worker| {
        let mut probe = Handle::new();
        let output = Arc::clone(&emitted);
        let mut inputs: [InputSession<u64, [i64; 3], isize>; 4] =
            worker.dataflow::<u64, _, _>(|scope| {
                let (mi, membership) = scope.new_collection::<[i64; 3], isize>();
                let (pi, permission) = scope.new_collection::<[i64; 3], isize>();
                let (di, direct) = scope.new_collection::<[i64; 3], isize>();
                let (ji, job) = scope.new_collection::<[i64; 3], isize>();
                let result = if shape == Shape::Access {
                    membership
                        .map(|[_, person, team]| (team, person))
                        .join(permission.map(|[_, team, resource]| (team, resource)))
                        .map(|(_, (person, resource))| vec![person, resource])
                        .concat(direct.map(|[_, person, resource]| vec![person, resource]))
                        .distinct()
                } else {
                    job.map(|[_, team, cost]| (team, cost))
                        .explode(|(team, cost)| Some((team, (1isize, cost as isize))))
                        .count_total()
                        .map(|(team, (count, sum))| vec![team, count as i64, sum as i64])
                };
                result
                    .consolidate()
                    .inspect(move |(row, _, weight)| {
                        *output.lock().entry(row.clone()).or_default() += *weight;
                    })
                    .probe_with(&mut probe);
                [mi, pi, di, ji]
            });
        let mut epoch = 0_u64;
        let mut rows = BTreeMap::<Vec<i64>, isize>::new();
        let _ = ready.send(());
        while let Ok(command) = receiver.lock().recv() {
            match command {
                Command::Apply(changes, reply) => {
                    for change in changes {
                        inputs[change.table].update([change.id, change.a, change.b], change.weight);
                    }
                    epoch += 1;
                    for input in &mut inputs {
                        input.advance_to(epoch);
                        input.flush();
                    }
                    while probe.less_than(&epoch) {
                        worker.step();
                    }
                    let changes = std::mem::take(&mut *emitted.lock());
                    let mut delta = Vec::new();
                    for (row, weight) in changes {
                        if weight == 0 {
                            continue;
                        }
                        let entry = rows.entry(row.clone()).or_default();
                        *entry += weight;
                        if *entry == 0 {
                            rows.remove(&row);
                        }
                        delta.push((row, weight));
                    }
                    let _ = reply.send(delta);
                }
                Command::Snapshot(reply) => {
                    let snapshot = rows
                        .iter()
                        .flat_map(|(row, weight)| {
                            std::iter::repeat_n(row.clone(), *weight as usize)
                        })
                        .collect();
                    let _ = reply.send(snapshot);
                }
                Command::Stop => break,
            }
        }
    });
}
