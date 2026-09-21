use tracing::Subscriber;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer};

use crate::OutputFormat;

#[derive(Clone, Debug)]
pub struct FormatConfig {
    pub format: OutputFormat,
    pub ansi: bool,
    pub target: bool,
    pub thread_names: bool,
    pub span_events: FmtSpan,
}

impl FormatConfig {
    pub fn standard(format: OutputFormat, ansi: bool) -> Self {
        Self {
            format,
            ansi,
            target: true,
            thread_names: false,
            span_events: FmtSpan::NONE,
        }
    }
}

pub const DEFAULT_FILTER_VARIABLE: &str = "HAFLEY_LOG";

/// Silence hides the defect this crate exists to catch, so an unset RUST_LOG
/// falls back to trace rather than to the caller's quieter preference.
pub fn env_filter(caller_filter: &str) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    match std::env::var(DEFAULT_FILTER_VARIABLE) {
        Ok(filter) if !filter.is_empty() => EnvFilter::new(filter),
        _ => EnvFilter::new(if caller_filter.is_empty() {
            "trace"
        } else {
            caller_filter
        }),
    }
}

/// The text baseline. With the feature off the layer is an identity, so a host
/// keeps its call site and the binary carries no formatter.
#[cfg(feature = "fmt")]
pub fn format_layer<S>(config: FormatConfig, writer: BoxMakeWriter) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    match config.format {
        OutputFormat::Human => tracing_subscriber::fmt::layer()
            .with_ansi(config.ansi)
            .with_target(config.target)
            .with_thread_names(config.thread_names)
            .with_span_events(config.span_events)
            .with_writer(writer)
            .boxed(),
        OutputFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .with_target(config.target)
            .with_thread_names(config.thread_names)
            .with_span_events(config.span_events)
            .with_writer(writer)
            .boxed(),
    }
}

#[cfg(not(feature = "fmt"))]
pub fn format_layer<S>(_config: FormatConfig, _writer: BoxMakeWriter) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::layer::Identity::new().boxed()
}