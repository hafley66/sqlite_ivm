//! `lab <vec|col|col-file|col-sqlite|vec-row|col-row|col-row-file|col-row-sqlite> <keys> <epochs> [budget]`: one arrangement per process,
//! so peak RSS is the arm's own.

use differential_dataflow::columnar::collection::Builder as ColBuilder;
use differential_dataflow::columnar::trace::spill::{self, BytesSource, BytesStore, SpillStats};
use differential_dataflow::columnar::trace::{Batcher as ColBatcher, Builder as ColTraceBuilder, Chunker as ColChunker, Spine as ColSpine};
use differential_dataflow::input::Input as _;
use differential_dataflow::operators::arrange::arrangement::arrange_core;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::probe::{Handle as ProbeHandle, Probe};
use timely::dataflow::operators::Input;
use timely::dataflow::InputHandle;

type Update = (u64, u64, u64, i64);
type RowUpdate = (Vec<i64>, Vec<i64>, u64, i64);

/// Keys spread over the u64 range so sorted chunks are not trivially compressible.
fn mix(k: u64) -> u64 {
    let x = k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^ (x >> 32)
}

/// lz4 blobs appended to one tempfile.
struct FileStore {
    file: Rc<std::cell::RefCell<std::fs::File>>,
    offset: u64,
}

struct FileSource {
    file: Rc<std::cell::RefCell<std::fs::File>>,
    offset: u64,
    compressed: usize,
    raw: usize,
}

impl BytesStore for FileStore {
    fn store(&mut self, bytes: &[u8]) -> Box<dyn BytesSource> {
        let compressed = lz4_flex::block::compress(bytes);
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(self.offset)).unwrap();
        file.write_all(&compressed).unwrap();
        let source = FileSource { file: self.file.clone(), offset: self.offset, compressed: compressed.len(), raw: bytes.len() };
        self.offset += compressed.len() as u64;
        Box::new(source)
    }
}

impl BytesSource for FileSource {
    fn load(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.compressed];
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(self.offset)).unwrap();
        file.read_exact(&mut buf).unwrap();
        lz4_flex::block::decompress(&buf, self.raw).unwrap()
    }
}

/// lz4 blobs as rows of one SQLite table; a source is its rowid.
struct SqliteStore {
    db: Rc<rusqlite::Connection>,
}

struct SqliteSource {
    db: Rc<rusqlite::Connection>,
    id: i64,
    raw: usize,
}

impl SqliteStore {
    fn open() -> Self {
        let path = std::env::temp_dir().join(format!("lab-dd-spill-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-8192;
            CREATE TABLE chunk(id INTEGER PRIMARY KEY, bytes BLOB NOT NULL);").unwrap();
        Self { db: Rc::new(db) }
    }
}

impl BytesStore for SqliteStore {
    fn store(&mut self, bytes: &[u8]) -> Box<dyn BytesSource> {
        let compressed = lz4_flex::block::compress(bytes);
        self.db.prepare_cached("INSERT INTO chunk(bytes) VALUES (?1)").unwrap().execute([&compressed]).unwrap();
        Box::new(SqliteSource { db: self.db.clone(), id: self.db.last_insert_rowid(), raw: bytes.len() })
    }
}

impl BytesSource for SqliteSource {
    fn load(&self) -> Vec<u8> {
        let blob: Vec<u8> = self.db.prepare_cached("SELECT bytes FROM chunk WHERE id = ?1").unwrap()
            .query_row([self.id], |row| row.get(0)).unwrap();
        lz4_flex::block::decompress(&blob, self.raw).unwrap()
    }
}

/// Records the trace holds after the last epoch: a dropped or empty trace reads 0.
fn report_len<Tr: differential_dataflow::trace::TraceReader>(trace: &mut Tr) {
    let mut records = 0usize;
    trace.map_batches(|batch| records += differential_dataflow::trace::BatchReader::len(batch));
    eprintln!("retained_records={records}");
}

fn peak_rss_mb() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let max = usage.ru_maxrss as f64;
    // macOS reports bytes, Linux kilobytes.
    if cfg!(target_os = "macos") { max / 1048576.0 } else { max / 1024.0 }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arm = args.get(1).cloned().unwrap_or_else(|| "col".into());
    let keys: u64 = args.get(2).map_or(2_000_000, |a| a.parse().unwrap());
    let epochs: u64 = args.get(3).map_or(8, |a| a.parse().unwrap());
    let budget: usize = args.get(4).map_or(250_000, |a| a.parse().unwrap());
    let per_epoch = keys / epochs;
    let stats = Arc::new(SpillStats::default());
    let start = std::time::Instant::now();
    let arm_run = arm.clone();
    let stats_run = stats.clone();
    timely::execute_directly(move |worker| {
        let mut probe: ProbeHandle<u64> = ProbeHandle::new();
        if arm_run == "vec-row" {
            let (mut input, mut trace) = worker.dataflow::<u64, _, _>(|scope| {
                let (input, collection) = scope.new_collection::<(Vec<i64>, Vec<i64>), i64>();
                let arranged = collection.arrange_by_key();
                arranged.stream.probe_with(&mut probe);
                (input, arranged.trace)
            });
            for epoch in 0..epochs {
                for k in epoch * per_epoch..(epoch + 1) * per_epoch {
                    let key = mix(k) as i64;
                    input.update((vec![key], vec![key, key & 0xff, k as i64]), 1);
                }
                input.advance_to(epoch + 1);
                input.flush();
                while probe.less_than(input.time()) { worker.step(); }
            }
            report_len(&mut trace);
            return;
        }
        if let Some(store) = arm_run.strip_prefix("col-row") {
            match store {
                "-file" => spill::install(budget, Box::new(FileStore { file: Rc::new(std::cell::RefCell::new(tempfile::tempfile().unwrap())), offset: 0 }), stats_run.clone()),
                "-sqlite" => spill::install(budget, Box::new(SqliteStore::open()), stats_run.clone()),
                "" => spill::uninstall(),
                other => panic!("unknown arm col-row{other}"),
            }
            let mut input = <InputHandle<u64, ColBuilder<RowUpdate>>>::new_with_builder();
            let mut trace = worker.dataflow::<u64, _, _>(|scope| {
                let stream = scope.input_from(&mut input);
                let arranged = arrange_core::<
                    _, _,
                    ColChunker<RowUpdate>,
                    ColBatcher<Vec<i64>, Vec<i64>, u64, i64>,
                    ColTraceBuilder<Vec<i64>, Vec<i64>, u64, i64>,
                    ColSpine<Vec<i64>, Vec<i64>, u64, i64>,
                >(stream, Pipeline, "LabArrangeRows");
                arranged.stream.probe_with(&mut probe);
                arranged.trace
            });
            for epoch in 0..epochs {
                for k in epoch * per_epoch..(epoch + 1) * per_epoch {
                    let key = mix(k) as i64;
                    input.send((vec![key], vec![key, key & 0xff, k as i64], epoch, 1));
                }
                input.advance_to(epoch + 1);
                input.flush();
                while probe.less_than(input.time()) { worker.step(); }
            }
            report_len(&mut trace);
            spill::uninstall();
            return;
        }
        if arm_run == "vec" {
            let (mut input, mut trace) = worker.dataflow::<u64, _, _>(|scope| {
                let (input, collection) = scope.new_collection::<(u64, u64), i64>();
                let arranged = collection.arrange_by_key();
                arranged.stream.probe_with(&mut probe);
                (input, arranged.trace)
            });
            for epoch in 0..epochs {
                for k in epoch * per_epoch..(epoch + 1) * per_epoch {
                    let key = mix(k);
                    input.update((key, key & 0xff), 1);
                }
                input.advance_to(epoch + 1);
                input.flush();
                while probe.less_than(input.time()) { worker.step(); }
            }
            report_len(&mut trace);
            return;
        }
        match arm_run.as_str() {
            "col-file" => spill::install(budget, Box::new(FileStore { file: Rc::new(std::cell::RefCell::new(tempfile::tempfile().unwrap())), offset: 0 }), stats_run.clone()),
            "col-sqlite" => spill::install(budget, Box::new(SqliteStore::open()), stats_run.clone()),
            "col" => spill::uninstall(),
            other => panic!("unknown arm {other}"),
        }
        let mut input = <InputHandle<u64, ColBuilder<Update>>>::new_with_builder();
        let mut trace = worker.dataflow::<u64, _, _>(|scope| {
            let stream = scope.input_from(&mut input);
            let arranged = arrange_core::<
                _, _,
                ColChunker<Update>,
                ColBatcher<u64, u64, u64, i64>,
                ColTraceBuilder<u64, u64, u64, i64>,
                ColSpine<u64, u64, u64, i64>,
            >(stream, Pipeline, "LabArrange");
            arranged.stream.probe_with(&mut probe);
            arranged.trace
        });
        for epoch in 0..epochs {
            for k in epoch * per_epoch..(epoch + 1) * per_epoch {
                let key = mix(k);
                input.send((key, key & 0xff, epoch, 1));
            }
            input.advance_to(epoch + 1);
            input.flush();
            while probe.less_than(input.time()) { worker.step(); }
        }
        report_len(&mut trace);
        spill::uninstall();
    });
    println!(
        "{arm}\tkeys={keys}\tepochs={epochs}\tbudget={budget}\tpeak_rss_mb={:.0}\twall_s={:.2}\tspilled_chunks={}\tspilled_records={}\tfetched_chunks={}",
        peak_rss_mb(),
        start.elapsed().as_secs_f64(),
        stats.spilled_chunks.load(Ordering::Relaxed),
        stats.spilled_records.load(Ordering::Relaxed),
        stats.fetched_chunks.load(Ordering::Relaxed),
    );
}
