use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

#[cfg(feature = "otlp-trace")]
use std::sync::OnceLock;
#[cfg(feature = "otlp-trace")]
use std::time::Duration;

#[cfg(feature = "otlp-trace")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "otlp-trace")]
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
#[cfg(feature = "otlp-trace")]
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};
#[cfg(feature = "otlp-trace")]
use opentelemetry_sdk::Resource;

use crate::Config;

#[cfg(feature = "otlp-trace")]
static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

/// The OTLP/HTTP layer, or `None` when `HAFLEY_OTLP_ENDPOINT` is unset.
///
/// The endpoint is the whole switch: without it the process keeps the
/// formatter-only subscriber and pays one failed `env::var` lookup.
#[cfg(feature = "otlp-trace")]
pub fn otlp_layer<S>(config: &Config) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let endpoint = std::env::var("HAFLEY_OTLP_ENDPOINT").ok()?;
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .ok()?;
    let batch = BatchConfigBuilder::default()
        .with_max_queue_size(2048)
        .with_max_export_batch_size(512)
        .with_scheduled_delay(Duration::from_millis(5000))
        .build();
    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(batch)
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .with_resource(
            Resource::builder()
                .with_service_name(config.service_name)
                .build(),
        )
        .build();
    let _ = PROVIDER.set(provider.clone());
    Some(
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("hafley"))
            .boxed(),
    )
}

#[cfg(not(feature = "otlp-trace"))]
pub fn otlp_layer<S>(_config: &Config) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

/// Flush the batch processor and the metric readers. Without this the last
/// scheduled delay of spans and the last metric interval are dropped at
/// process exit.
#[cfg(feature = "otlp-trace")]
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
    crate::instruments::stop();
    crate::instruments::shutdown();
}

#[cfg(not(feature = "otlp-trace"))]
pub fn shutdown() {
    crate::instruments::stop();
    crate::instruments::shutdown();
}