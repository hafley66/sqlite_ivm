//! When a sink writes. One enum selects the strategy for every sink, so a
//! measured row never prices a per-sink hack.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The row bound on the drain channel. It is the memory ceiling on one sink: a
/// stalled drain cannot grow the backlog past this many rows.
pub const DRAIN_ROW_BOUND: usize = 4096;

/// The rows one drain pass writes before it reads again, so the drain holds a
/// sink for no longer than a bounded batch.
pub const DRAIN_BATCH_BOUND: usize = 512;

/// How long the drain waits for the next row before writing what it holds.
pub const DRAIN_WAIT: Duration = Duration::from_millis(250);

/// The rows an on-commit buffer holds before it writes ahead of its commit
/// point, so a host that never commits cannot grow it without bound.
pub const COMMIT_ROW_BOUND: usize = 4096;

/// The polls a `flush` spends waiting for in-flight rows, one millisecond
/// apart. A flush that waits forever is a hang, not a flush.
pub const FLUSH_WAIT_STEPS: usize = 5000;

/// The cache entries a dictionary encoder keeps before it drops the cache and
/// re-reads the side tables. It is the memory ceiling on interning.
pub const DICTIONARY_CACHE_BOUND: usize = 8192;

/// The named diagnostic the drain raises when its bound is hit.
pub const DRAIN_BOUND_DIAGNOSTIC: &str = "observe.drain.bound";

/// When a sink writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flush {
    /// On every row, on the emitting thread.
    #[default]
    Immediate,
    /// Buffered, written by a background drain at a bounded interval or a
    /// bounded buffer size, whichever comes first.
    Drain,
    /// Buffered, written when the host declares a commit point.
    OnCommit,
}

pub const FLUSH_VARIABLE: &str = "HAFLEY_FLUSH";

impl Flush {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Drain => "drain",
            Self::OnCommit => "on-commit",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ParseFlushError> {
        match value {
            "immediate" | "inline" => Ok(Self::Immediate),
            "drain" | "buffered" => Ok(Self::Drain),
            "on-commit" | "commit" => Ok(Self::OnCommit),
            other => Err(ParseFlushError(other.to_owned())),
        }
    }

    pub fn from_env() -> Self {
        match std::env::var(FLUSH_VARIABLE) {
            Ok(value) => Self::parse(&value).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseFlushError(pub String);

impl fmt::Display for ParseFlushError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown flush strategy {:?}; expected immediate, drain or on-commit",
            self.0
        )
    }
}

impl std::error::Error for ParseFlushError {}

/// One log record, as the sink sees it. The name is the nearest enclosing span,
/// so a span name repeats across every row its events produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub ts_ns: i64,
    pub level: &'static str,
    pub name: String,
    pub target: String,
    pub file: String,
    pub line: u32,
    pub fields: Vec<(String, String)>,
}

/// Where rows land. One `write` takes a batch so the drain, the commit path and
/// the inline path share a single write path.
pub trait Sink: Send + Sync {
    fn label(&self) -> &'static str;
    fn write(&self, rows: &[Row]);
}

/// A sink plus its flush strategy.
pub struct Writer {
    inner: Arc<dyn Sink>,
    mode: Mode,
    dropped: AtomicU64,
}

enum Mode {
    Immediate,
    Drain {
        rows: SyncSender<Row>,
        in_flight: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        drain: Mutex<Option<JoinHandle<()>>>,
    },
    OnCommit {
        buffer: Mutex<Vec<Row>>,
    },
}

impl Writer {
    pub fn new(inner: Arc<dyn Sink>, flush: Flush) -> Self {
        let mode = match flush {
            Flush::Immediate => Mode::Immediate,
            Flush::Drain => {
                let (rows, receiver) = sync_channel(DRAIN_ROW_BOUND);
                let in_flight = Arc::new(AtomicUsize::new(0));
                let stop = Arc::new(AtomicBool::new(false));
                let sink = Arc::clone(&inner);
                let counter = Arc::clone(&in_flight);
                let halt = Arc::clone(&stop);
                let drain = thread::Builder::new()
                    .name("hafley-observe-drain".to_owned())
                    .spawn(move || drain_loop(receiver, sink, counter, halt))
                    .ok();
                Mode::Drain {
                    rows,
                    in_flight,
                    stop,
                    drain: Mutex::new(drain),
                }
            }
            Flush::OnCommit => Mode::OnCommit {
                buffer: Mutex::new(Vec::new()),
            },
        };
        Self {
            inner,
            mode,
            dropped: AtomicU64::new(0),
        }
    }

    pub fn flush_strategy(&self) -> &'static str {
        match self.mode {
            Mode::Immediate => Flush::Immediate.as_str(),
            Mode::Drain { .. } => Flush::Drain.as_str(),
            Mode::OnCommit { .. } => Flush::OnCommit.as_str(),
        }
    }

    pub fn label(&self) -> &'static str {
        self.inner.label()
    }

    /// Rows the drain could not buffer because its bound was already hit.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn write(&self, row: Row) {
        match &self.mode {
            Mode::Immediate => self.inner.write(std::slice::from_ref(&row)),
            Mode::Drain {
                rows,
                in_flight,
                stop,
                ..
            } => {
                in_flight.fetch_add(1, Ordering::AcqRel);
                match rows.try_send(row) {
                    Ok(()) => {}
                    Err(TrySendError::Full(row)) => {
                        // The bound is the ceiling the drain promised. Hitting
                        // it stops the drain by name and hands this row to the
                        // emitting thread, so nothing is lost silently.
                        in_flight.fetch_sub(1, Ordering::AcqRel);
                        stop.store(true, Ordering::Release);
                        self.report_bound();
                        self.inner.write(std::slice::from_ref(&row));
                    }
                    Err(TrySendError::Disconnected(row)) => {
                        in_flight.fetch_sub(1, Ordering::AcqRel);
                        self.inner.write(std::slice::from_ref(&row));
                    }
                }
            }
            Mode::OnCommit { buffer } => {
                let mut buffered = match buffer.lock() {
                    Ok(buffered) => buffered,
                    Err(poisoned) => poisoned.into_inner(),
                };
                buffered.push(row);
                if buffered.len() >= COMMIT_ROW_BOUND {
                    self.inner.write(&buffered);
                    buffered.clear();
                }
            }
        }
    }

    fn report_bound(&self) {
        if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::error!(
                diagnostic = DRAIN_BOUND_DIAGNOSTIC,
                bound = DRAIN_ROW_BOUND,
                "drain buffer bound hit; the drain stopped and writes go inline"
            );
        }
    }

    /// Write what the strategy holds. A no-op while nothing is held.
    pub fn flush(&self) {
        match &self.mode {
            Mode::Immediate => {}
            Mode::Drain { in_flight, .. } => {
                // budget: FLUSH_WAIT_STEPS polls of one millisecond
                for _ in 0..FLUSH_WAIT_STEPS {
                    if in_flight.load(Ordering::Acquire) == 0 {
                        return;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Mode::OnCommit { buffer } => {
                let mut buffered = match buffer.lock() {
                    Ok(buffered) => buffered,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if !buffered.is_empty() {
                    self.inner.write(&buffered);
                    buffered.clear();
                }
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Mode::Drain { stop, drain, .. } = &self.mode {
            stop.store(true, Ordering::Release);
            if let Ok(mut slot) = drain.lock() {
                if let Some(handle) = slot.take() {
                    let _ = handle.join();
                }
            }
        }
        self.flush();
    }
}

fn drain_loop(
    rows: Receiver<Row>,
    sink: Arc<dyn Sink>,
    in_flight: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    let mut batch: Vec<Row> = Vec::with_capacity(DRAIN_BATCH_BOUND);
    // budget: DRAIN_WAIT per wait, DRAIN_BATCH_BOUND rows per write
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match rows.recv_timeout(DRAIN_WAIT) {
            Ok(row) => {
                batch.push(row);
                if batch.len() >= DRAIN_BATCH_BOUND {
                    write_batch(&sink, &in_flight, &mut batch);
                }
            }
            Err(RecvTimeoutError::Timeout) => write_batch(&sink, &in_flight, &mut batch),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // The channel holds at most DRAIN_ROW_BOUND rows, so this tail is finite.
    for row in rows.try_iter() {
        batch.push(row);
        if batch.len() >= DRAIN_BATCH_BOUND {
            write_batch(&sink, &in_flight, &mut batch);
        }
    }
    write_batch(&sink, &in_flight, &mut batch);
}

fn write_batch(sink: &Arc<dyn Sink>, in_flight: &AtomicUsize, batch: &mut Vec<Row>) {
    if batch.is_empty() {
        return;
    }
    sink.write(batch);
    in_flight.fetch_sub(batch.len(), Ordering::AcqRel);
    batch.clear();
}