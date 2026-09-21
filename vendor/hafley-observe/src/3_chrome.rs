use std::path::PathBuf;
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

#[cfg(feature = "chrome")]
use std::sync::Mutex;
#[cfg(feature = "chrome")]
use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
#[cfg(feature = "chrome")]
use tracing_subscriber::EnvFilter;

pub const TRACE_PATH_VARIABLE: &str = "HAFLEY_TRACE";

// A host may std::process::exit, which skips Drop, so the guard lives in a
// process-global slot and finish_trace is called explicitly at every exit site.
#[cfg(feature = "chrome")]
static TRACE_GUARD: Mutex<Option<FlushGuard>> = Mutex::new(None);

pub fn trace_path() -> Option<PathBuf> {
    std::env::var_os(TRACE_PATH_VARIABLE)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The chrome timeline layer, or `None` when no path is set or the feature is
/// off.
#[cfg(feature = "chrome")]
pub fn chrome_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let path = trace_path()?;
    let (layer, guard) = ChromeLayerBuilder::new()
        .file(path)
        .include_args(true)
        .build();
    // The export carries its own filter so a quiet stderr default cannot
    // silently produce an empty timeline.
    let layer = layer.with_filter(EnvFilter::new(
        std::env::var("HAFLEY_TRACE_FILTER").unwrap_or_else(|_| "trace".to_string()),
    ));
    if let Ok(mut slot) = TRACE_GUARD.lock() {
        *slot = Some(guard);
    }
    Some(layer.boxed())
}

#[cfg(not(feature = "chrome"))]
pub fn chrome_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

#[cfg(feature = "chrome")]
pub fn finish_trace() {
    if let Ok(mut slot) = TRACE_GUARD.lock() {
        slot.take();
    }
}

#[cfg(not(feature = "chrome"))]
pub fn finish_trace() {}