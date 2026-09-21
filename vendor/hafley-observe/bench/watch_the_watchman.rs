//! The watch-the-watchman harness.
//!
//! One workload, one measurement. The binary is built twice per candidate, with
//! the layer off and with it on, and the difference between the two builds is
//! the price of the layer. This process prints one row; the recipe does the
//! arithmetic.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tracing_subscriber::layer::SubscriberExt as _;

use hafley_observe::flush::{Flush, Writer};
use hafley_observe::{instruments, rusage, Config, FormatConfig, OutputFormat};
#[cfg(feature = "sqlite-sink")]
use hafley_observe::Sink as _;

// The tracked allocator is a property of the binary, so the harness installs
// it. With the feature off the line does not exist and the default allocator
// stands.
#[cfg(feature = "tracy-alloc")]
hafley_observe::tracy_allocator!(TRACY_ALLOC);

/// The workload, in one place. The shape is a document: an outer paragraph
/// span, a line span, and a hot token span. Every count below is a constant,
/// so two runs with the same binaries emit the same events.
const PARAGRAPHS: usize = 200;
const LINES_PER_PARAGRAPH: usize = 5;
const TOKENS_PER_LINE: usize = 20;
const GLYPHS_PER_TOKEN: u64 = 7;
const LINE_WIDTH: u64 = 80;

/// Events the workload emits. Every token emits one.
pub const EVENTS: usize = PARAGRAPHS * LINES_PER_PARAGRAPH * TOKENS_PER_LINE;

/// The target every workload event carries.
const WORKLOAD_TARGET: &str = "engine.layout";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SinkChoice {
    None,
    Dictionary,
    Text,
}

impl SinkChoice {
    fn parse(value: &str) -> Self {
        match value {
            "dictionary" | "dict" => Self::Dictionary,
            "text" => Self::Text,
            _ => Self::None,
        }
    }
}

struct Args {
    feature: String,
    strategy: Flush,
    sink: SinkChoice,
    db: Option<PathBuf>,
}

fn args() -> Args {
    let mut feature = "none".to_owned();
    let mut strategy = Flush::Immediate;
    let mut sink = SinkChoice::None;
    let mut db = None;
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--feature" => feature = argv.next().unwrap_or_default(),
            "--strategy" => {
                strategy = argv
                    .next()
                    .and_then(|value| Flush::parse(&value).ok())
                    .unwrap_or_default()
            }
            "--sink" => sink = SinkChoice::parse(&argv.next().unwrap_or_default()),
            "--db" => db = argv.next().map(PathBuf::from),
            other => {
                // @eprintln-ok: a CLI usage line, not a log.
                eprintln!("unknown argument {other}");
            }
        }
    }
    Args {
        feature,
        strategy,
        sink,
        db,
    }
}

fn main() {
    let args = args();
    let config = Config {
        service_name: "watch-the-watchman",
        service_version: env!("CARGO_PKG_VERSION"),
        default_filter: "trace",
        format: OutputFormat::Human,
        ansi: false,
    };

    let writer = args
        .db
        .as_deref()
        .filter(|_| args.sink != SinkChoice::None)
        .and_then(|path| open_log(path, args.sink, args.strategy));

    instruments::install(&config);
    instruments::start(&config);

    let subscriber = stack(&config, writer.clone());

    let before = rusage::sample();
    let clock = Instant::now();
    tracing::subscriber::with_default(subscriber, workload);
    if let Some(writer) = &writer {
        // The deferred work a drain or a commit strategy owes is part of its
        // cost, so the flush is inside the timed region.
        writer.flush();
    }
    let wall = clock.elapsed();
    let after = rusage::sample();

    hafley_observe::shutdown();

    let (label, rows, bytes) = match (&args.db, args.sink) {
        (Some(path), choice) if choice != SinkChoice::None => sink_report(path, choice),
        _ => ("none", 0, 0),
    };
    let seconds = wall.as_secs_f64();
    let events_per_sec = if seconds > 0.0 {
        EVENTS as f64 / seconds
    } else {
        0.0
    };
    let line = format!(
        "{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{:.0}\t{}\t{}\t{:.1}",
        args.feature,
        args.strategy.as_str(),
        layer_names(),
        label,
        wall.as_secs_f64() * 1000.0,
        after.peak_rss_bytes.unwrap_or_default(),
        after
            .disk_read_bytes
            .unwrap_or_default()
            .saturating_sub(before.disk_read_bytes.unwrap_or_default()),
        after
            .disk_write_bytes
            .unwrap_or_default()
            .saturating_sub(before.disk_write_bytes.unwrap_or_default()),
        EVENTS,
        events_per_sec,
        rows,
        bytes,
        seconds * 1_000_000_000.0 / EVENTS as f64,
    );
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// The layers this build carries, by name. The off build carries none.
fn layer_names() -> String {
    let mut names: Vec<&str> = Vec::new();
    if cfg!(feature = "fmt") {
        names.push("fmt");
    }
    if cfg!(feature = "chrome") {
        names.push("chrome");
    }
    if cfg!(feature = "otlp-trace") {
        names.push("otlp-trace");
    }
    if cfg!(feature = "otlp-metrics") {
        names.push("otlp-metrics");
    }
    if cfg!(feature = "sysmetrics") {
        names.push("sysmetrics");
    }
    if cfg!(feature = "procmetrics") {
        names.push("procmetrics");
    }
    if cfg!(feature = "metrics-ctx") {
        names.push("metrics-ctx");
    }
    if cfg!(feature = "tracy") {
        names.push("tracy");
    }
    if cfg!(feature = "tracy-alloc") {
        names.push("tracy-alloc");
    }
    if cfg!(feature = "rusage") {
        names.push("rusage");
    }
    if cfg!(feature = "sqlite-sink") {
        names.push("sqlite-sink");
    }
    if names.is_empty() {
        return "none".to_owned();
    }
    names.join(",")
}

fn stack(
    config: &Config,
    writer: Option<Arc<Writer>>,
) -> impl tracing::Subscriber + Send + Sync + 'static {
    // Every layer is attached in one chain, so the compiler sees the concrete
    // type at each step. The off build attaches the identity of each.
    let sink = writer.map(hafley_observe::SinkLayer::new);
    tracing_subscriber::registry()
        .with(hafley_observe::format_layer(
            FormatConfig::standard(config.format, config.ansi),
            tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::sink),
        ))
        .with(hafley_observe::chrome_layer())
        .with(instruments::context_layer())
        .with(instruments::span_layer(config))
        .with(instruments::proc_layer())
        .with(hafley_observe::tracy_layer())
        .with(hafley_observe::rusage_layer())
        .with(hafley_observe::otlp_layer(config))
        .with(sink)
}

/// The traced work. Deterministic: no clock drives a decision, and no random
/// number is drawn.
fn workload() {
    for paragraph in 0..PARAGRAPHS {
        let span = tracing::info_span!(
            target: WORKLOAD_TARGET,
            "paragraph",
            paragraph,
            glyphs = LINES_PER_PARAGRAPH as u64 * TOKENS_PER_LINE as u64 * GLYPHS_PER_TOKEN
        );
        let _paragraph = span.enter();
        for line in 0..LINES_PER_PARAGRAPH {
            let span = tracing::debug_span!(target: WORKLOAD_TARGET, "line", line, width = LINE_WIDTH);
            let _line = span.enter();
            for token in 0..TOKENS_PER_LINE {
                let span = tracing::trace_span!(target: WORKLOAD_TARGET, "token", token);
                let _token = span.enter();
                tracing::trace!(
                    target: WORKLOAD_TARGET,
                    glyphs = GLYPHS_PER_TOKEN,
                    width = LINE_WIDTH,
                    "token placed"
                );
            }
        }
    }
}

/// The sink the layer writes through, when one was asked for. The handle is
/// the writer, so nothing else about the sink leaks into the timing path.
#[cfg(feature = "sqlite-sink")]
fn open_log(path: &Path, choice: SinkChoice, strategy: Flush) -> Option<Arc<Writer>> {
    use hafley_observe::sqlite::{self, Encoding};
    let _ = std::fs::remove_file(path);
    let encoding = match choice {
        SinkChoice::Text => Encoding::Text,
        _ => Encoding::Dictionary,
    };
    // @eprintln-ok: a CLI progress line, not a log.
    eprintln!("sink {} -> {}", encoding_name(encoding), path.display());
    sqlite::open(path, encoding, strategy).ok().map(|log| log.writer)
}

#[cfg(not(feature = "sqlite-sink"))]
fn open_log(_path: &Path, _choice: SinkChoice, _strategy: Flush) -> Option<Arc<Writer>> {
    None
}

/// What the sink holds after the run, read back through the same public
/// surface that wrote it.
#[cfg(feature = "sqlite-sink")]
fn sink_report(path: &Path, choice: SinkChoice) -> (&'static str, i64, i64) {
    use hafley_observe::sqlite::{self, Encoding};
    let encoding = match choice {
        SinkChoice::Text => Encoding::Text,
        _ => Encoding::Dictionary,
    };
    match sqlite::open(path, encoding, Flush::Immediate) {
        Ok(log) => (log.sink.label(), log.sink.rows(), log.sink.bytes()),
        Err(_) => ("none", 0, 0),
    }
}

#[cfg(not(feature = "sqlite-sink"))]
fn sink_report(_path: &Path, _choice: SinkChoice) -> (&'static str, i64, i64) {
    ("none", 0, 0)
}

#[cfg(feature = "sqlite-sink")]
fn encoding_name(encoding: hafley_observe::sqlite::Encoding) -> &'static str {
    match encoding {
        hafley_observe::sqlite::Encoding::Dictionary => "dictionary",
        hafley_observe::sqlite::Encoding::Text => "text",
    }
}