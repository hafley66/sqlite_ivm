#[path = "0_types.rs"]
mod _0_types;
#[path = "1_format.rs"]
mod _1_format;
#[path = "1_init.rs"]
mod _1_init;
#[path = "2_otlp.rs"]
mod _2_otlp;
#[path = "3_chrome.rs"]
mod _3_chrome;
#[path = "4_counts.rs"]
mod _4_counts;
#[path = "6_flush.rs"]
pub mod flush;
#[path = "9_metrics.rs"]
pub mod instruments;
#[path = "8_rusage.rs"]
pub mod rusage;
#[path = "7_sink.rs"]
pub mod sink;
#[cfg(feature = "sqlite-sink")]
#[path = "5_sqlite.rs"]
pub mod sqlite;
#[path = "10_tracy.rs"]
pub mod tracy;

pub use _0_types::{Config, OutputFormat, ParseOutputFormatError};
pub use _1_format::{env_filter, format_layer, FormatConfig, DEFAULT_FILTER_VARIABLE};
pub use _1_init::{init, init_with_writer, startup};
pub use _2_otlp::shutdown;
pub use _3_chrome::{chrome_layer, finish_trace, trace_path, TRACE_PATH_VARIABLE};
pub use _4_counts::{
    assert_growth, observed_growth, CountRecorder, EventStats, EventSums, FieldStats, Growth,
    SpanCounts,
};
pub use flush::{Flush, ParseFlushError, Row, Sink, Writer};
pub use instruments::{proc_layer, span_layer};
pub use rusage::{layer as rusage_layer, sample as process_sample, Usage};
pub use sink::SinkLayer;
pub use tracy::layer as tracy_layer;

pub use _2_otlp::otlp_layer;

/// The target the usage records carry.
pub const RUSAGE_TARGET: &str = "rusage";

/// The relational log sink for a host, or `None` when the database path is
/// unset or the feature is off.
#[cfg(feature = "sqlite-sink")]
pub(crate) fn log_sink_layer<S>(
    flush: Flush,
) -> Option<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
{
    sqlite::log_layer(flush)
}

#[cfg(not(feature = "sqlite-sink"))]
pub(crate) fn log_sink_layer<S>(
    _flush: Flush,
) -> Option<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
{
    None
}
