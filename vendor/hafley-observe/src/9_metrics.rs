//! The metrics pipelines. Each one is a bought library plus the glue a layer
//! needs, never a second telemetry stack.

use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::Config;

/// The variable that names the OTLP metrics endpoint. When it is unset the
/// traces endpoint is reused with its signal path swapped.
pub const METRICS_ENDPOINT_VARIABLE: &str = "HAFLEY_OTLP_METRICS_ENDPOINT";

pub const SPAN_COUNT_METRIC: &str = "observe.span.count";
pub const SPAN_DURATION_METRIC: &str = "observe.span.duration_ms";

/// The distinct instruments the `metrics` facade may register before the
/// bridge stops adding new ones. A label set that grows without bound must not
/// grow an instrument table without bound.
pub const INSTRUMENT_CARDINALITY_BOUND: usize = 512;

/// Samples the process observer takes before it stops.
pub const OBSERVER_SAMPLES: usize = 2;

/// Time between observer samples.
pub const OBSERVER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Samples the process collector takes between span closes.
pub const PROC_SAMPLE_EVERY_SPANS: u64 = 16;

#[cfg(feature = "otlp-metrics")]
mod pipeline {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Instant;

    use metrics::{CounterFn, GaugeFn, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit};
    #[cfg(feature = "metrics-ctx")]
    use metrics_util::layers::Layer as RecorderLayer;
    use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, MeterProvider as _};
    use opentelemetry::{KeyValue, Value};
    use opentelemetry_otlp::{MetricExporter, WithExportConfig as _};
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::Resource;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::registry::LookupSpan;

    use super::{Config, INSTRUMENT_CARDINALITY_BOUND, METRICS_ENDPOINT_VARIABLE, SPAN_COUNT_METRIC, SPAN_DURATION_METRIC};

    static PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();

    fn endpoint() -> Option<String> {
        if let Ok(value) = std::env::var(METRICS_ENDPOINT_VARIABLE) {
            if !value.is_empty() {
                return Some(value);
            }
        }
        let traces = std::env::var("HAFLEY_OTLP_ENDPOINT").ok()?;
        if traces.is_empty() {
            return None;
        }
        match traces.strip_suffix("/v1/traces") {
            Some(root) => Some(format!("{root}/v1/metrics")),
            None => Some(format!("{}/v1/metrics", traces.trim_end_matches('/'))),
        }
    }

    /// The meter provider, built once from the endpoint. `None` keeps the
    /// process on the formatter-only subscriber.
    pub fn provider(service_name: &'static str) -> Option<SdkMeterProvider> {
        if let Some(provider) = PROVIDER.get() {
            return Some(provider.clone());
        }
        let exporter = MetricExporter::builder()
            .with_http()
            .with_endpoint(endpoint()?)
            .build()
            .ok()?;
        let built = SdkMeterProvider::builder()
            .with_periodic_exporter(exporter)
            .with_resource(Resource::builder().with_service_name(service_name).build())
            .build();
        let _ = PROVIDER.set(built);
        PROVIDER.get().cloned()
    }

    pub fn meter(config: &Config) -> Option<Meter> {
        provider(config.service_name).map(|provider| provider.meter("hafley-observe"))
    }

    /// Flush the readers. Without this the last interval of instruments is
    /// dropped at process exit.
    pub fn shutdown() {
        if let Some(provider) = PROVIDER.get() {
            let _ = provider.shutdown();
        }
    }

    /// A span counter and a span duration histogram, the two instruments a
    /// metrics pipeline records without a caller writing anything.
    pub struct SpanMetricsLayer {
        spans: Counter<u64>,
        duration: Histogram<f64>,
    }

    impl SpanMetricsLayer {
        pub fn new(meter: Meter) -> Self {
            Self {
                spans: meter.u64_counter(SPAN_COUNT_METRIC).build(),
                duration: meter.f64_histogram(SPAN_DURATION_METRIC).build(),
            }
        }
    }

    #[derive(Debug)]
    struct StartedAt(Instant);

    impl<S> Layer<S> for SpanMetricsLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            _attrs: &tracing::span::Attributes<'_>,
            id: &tracing::Id,
            ctx: Context<'_, S>,
        ) {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(StartedAt(Instant::now()));
            }
        }

        fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
            let Some(span) = ctx.span(&id) else {
                return;
            };
            let name = span.name().to_owned();
            let attributes = [KeyValue::new("span.name", name)];
            self.spans.add(1, &attributes);
            let elapsed = span
                .extensions()
                .get::<StartedAt>()
                .map(|started| started.0.elapsed().as_secs_f64() * 1000.0)
                .unwrap_or_default();
            self.duration.record(elapsed, &attributes);
        }
    }

    /// One instrument per distinct label set, bounded in count.
    type Registry<T> = HashMap<String, Vec<(String, Arc<T>)>>;

    /// The `metrics` facade over the meter provider. Three candidates need a
    /// recorder: the process collector, and the tracing-context layer.
    pub struct Bridge {
        meter: Meter,
        counters: Mutex<Registry<FacadeCounter>>,
        gauges: Mutex<Registry<FacadeGauge>>,
        histograms: Mutex<Registry<FacadeHistogram>>,
    }

    impl Bridge {
        pub fn new(meter: Meter) -> Self {
            Self {
                meter,
                counters: Mutex::new(HashMap::new()),
                gauges: Mutex::new(HashMap::new()),
                histograms: Mutex::new(HashMap::new()),
            }
        }
    }

    fn attributes(key: &Key) -> (String, Vec<KeyValue>) {
        let pairs: Vec<(String, String)> = key
            .labels()
            .map(|label| (label.key().to_string(), label.value().to_string()))
            .collect();
        let label = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(";");
        let values = pairs
            .into_iter()
            .map(|(name, value)| KeyValue::new(name, Value::from(value)))
            .collect();
        (label, values)
    }

    fn bounded<T: Clone>(
        entries: &mut HashMap<String, Vec<(String, T)>>,
        name: &str,
        label: String,
        entry: T,
    ) -> Option<T> {
        let count: usize = entries.values().map(Vec::len).sum();
        if count >= INSTRUMENT_CARDINALITY_BOUND {
            return None;
        }
        entries
            .entry(name.to_owned())
            .or_default()
            .push((label, entry.clone()));
        Some(entry)
    }

    fn cached<T: Clone>(
        entries: &HashMap<String, Vec<(String, T)>>,
        name: &str,
        label: &str,
    ) -> Option<T> {
        entries
            .get(name)
            .and_then(|row| row.iter().find(|(key, _)| key == label))
            .map(|(_, value)| value.clone())
    }

    struct FacadeCounter {
        counter: Counter<u64>,
        attributes: Vec<KeyValue>,
        last: AtomicU64,
    }

    impl CounterFn for FacadeCounter {
        fn increment(&self, value: u64) {
            self.counter.add(value, &self.attributes);
        }

        fn absolute(&self, value: u64) {
            // The facade defines absolute as "at least this". A cumulative sum
            // takes the step, not the total.
            let previous = self.last.swap(value, Ordering::AcqRel);
            if value > previous {
                self.counter.add(value - previous, &self.attributes);
            }
        }
    }

    struct FacadeGauge {
        gauge: Gauge<f64>,
        attributes: Vec<KeyValue>,
        current: AtomicU64,
    }

    impl FacadeGauge {
        fn record(&self, value: f64) {
            self.current.store(value.to_bits(), Ordering::Release);
            self.gauge.record(value, &self.attributes);
        }
    }

    impl GaugeFn for FacadeGauge {
        fn increment(&self, value: f64) {
            let previous = f64::from_bits(self.current.load(Ordering::Acquire));
            self.record(previous + value);
        }

        fn decrement(&self, value: f64) {
            let previous = f64::from_bits(self.current.load(Ordering::Acquire));
            self.record(previous - value);
        }

        fn set(&self, value: f64) {
            self.record(value);
        }
    }

    struct FacadeHistogram {
        histogram: Histogram<f64>,
        attributes: Vec<KeyValue>,
    }

    impl HistogramFn for FacadeHistogram {
        fn record(&self, value: f64) {
            self.histogram.record(value, &self.attributes);
        }
    }

    impl Recorder for Bridge {
        fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}
        fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}
        fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

        fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> metrics::Counter {
            let name = key.name().to_string();
            let (label, attributes) = attributes(key);
            let mut entries = self.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(counter) = cached(&entries, &name, &label) {
                return metrics::Counter::from_arc(counter);
            }
            let counter = Arc::new(FacadeCounter {
                counter: self.meter.u64_counter(name.clone()).build(),
                attributes,
                last: AtomicU64::new(0),
            });
            match bounded(&mut entries, &name, label, counter) {
                Some(counter) => metrics::Counter::from_arc(counter),
                None => metrics::Counter::noop(),
            }
        }

        fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> metrics::Gauge {
            let name = key.name().to_string();
            let (label, attributes) = attributes(key);
            let mut entries = self.gauges.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(gauge) = cached(&entries, &name, &label) {
                return metrics::Gauge::from_arc(gauge);
            }
            let gauge = Arc::new(FacadeGauge {
                gauge: self.meter.f64_gauge(name.clone()).build(),
                attributes,
                current: AtomicU64::new(0),
            });
            match bounded(&mut entries, &name, label, gauge) {
                Some(gauge) => metrics::Gauge::from_arc(gauge),
                None => metrics::Gauge::noop(),
            }
        }

        fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> metrics::Histogram {
            let name = key.name().to_string();
            let (label, attributes) = attributes(key);
            let mut entries = self.histograms.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(histogram) = cached(&entries, &name, &label) {
                return metrics::Histogram::from_arc(histogram);
            }
            let histogram = Arc::new(FacadeHistogram {
                histogram: self.meter.f64_histogram(name.clone()).build(),
                attributes,
            });
            match bounded(&mut entries, &name, label, histogram) {
                Some(histogram) => metrics::Histogram::from_arc(histogram),
                None => metrics::Histogram::noop(),
            }
        }
    }

    /// Install the recorder. Returns false when a recorder is already set,
    /// which a process that initialized twice would hit.
    #[cfg(feature = "metrics-ctx")]
    pub fn install_recorder(meter: Meter) -> bool {
        use metrics_tracing_context::TracingContextLayer;
        let recorder = TracingContextLayer::all().layer(Bridge::new(meter));
        metrics::set_global_recorder(recorder).is_ok()
    }

    #[cfg(not(feature = "metrics-ctx"))]
    pub fn install_recorder(meter: Meter) -> bool {
        metrics::set_global_recorder(Bridge::new(meter)).is_ok()
    }

    /// The tracing side of the tracing-context layer: span fields become
    /// metric labels for every instrument the facade records.
    #[cfg(feature = "metrics-ctx")]
    pub fn context_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
    where
        S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
    {
        Some(metrics_tracing_context::MetricsLayer::default().boxed())
    }

    #[cfg(not(feature = "metrics-ctx"))]
    pub fn context_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
    where
        S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
    {
        None
    }

    /// Samples the process through the facade on a bounded cadence.
    #[cfg(feature = "procmetrics")]
    pub struct ProcMetricsLayer {
        collector: metrics_process::Collector,
        seen: AtomicU64,
    }

    #[cfg(feature = "procmetrics")]
    impl Default for ProcMetricsLayer {
        fn default() -> Self {
            Self::new()
        }
    }

    #[cfg(feature = "procmetrics")]
    impl ProcMetricsLayer {
        pub fn new() -> Self {
            Self {
                collector: metrics_process::Collector::new("process_"),
                seen: AtomicU64::new(0),
            }
        }
    }

    #[cfg(feature = "procmetrics")]
    impl<S> Layer<S> for ProcMetricsLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_close(&self, _id: tracing::Id, _ctx: Context<'_, S>) {
            let seen = self.seen.fetch_add(1, Ordering::AcqRel) + 1;
            if seen.is_multiple_of(super::PROC_SAMPLE_EVERY_SPANS) {
                self.collector.collect();
            }
        }
    }

    /// Runs the bought process observer on its own runtime. The sample count
    /// is bounded, so the thread ends.
    #[cfg(feature = "sysmetrics")]
    pub struct Observer {
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(feature = "sysmetrics")]
    impl Observer {
        pub fn finish(mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[cfg(feature = "sysmetrics")]
    pub fn start_observer(meter: Meter) -> Option<Observer> {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let halt = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("hafley-observe-sysmetrics".to_owned())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
                    return;
                };
                // budget: OBSERVER_SAMPLES passes, OBSERVER_INTERVAL apart
                for _ in 0..super::OBSERVER_SAMPLES {
                    if halt.load(Ordering::Acquire) {
                        break;
                    }
                    let _ = runtime.block_on(opentelemetry_system_metrics::init_process_observer_once(
                        meter.clone(),
                    ));
                    std::thread::sleep(super::OBSERVER_INTERVAL);
                }
            })
            .ok()?;
        Some(Observer {
            stop,
            thread: Some(thread),
        })
    }
}

#[cfg(feature = "otlp-metrics")]
pub use pipeline::{
    context_layer, install_recorder, meter, provider, shutdown, SpanMetricsLayer,
};

#[cfg(feature = "sysmetrics")]
pub use pipeline::{start_observer, Observer};

#[cfg(feature = "procmetrics")]
pub use pipeline::ProcMetricsLayer;

#[cfg(not(feature = "otlp-metrics"))]
pub fn shutdown() {}

/// The span metrics layer, or `None` when the endpoint is unset.
#[cfg(feature = "otlp-metrics")]
pub fn span_layer<S>(
    config: &Config,
) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    meter(config).map(|meter| Layer::boxed(SpanMetricsLayer::new(meter)))
}

#[cfg(not(feature = "otlp-metrics"))]
pub fn span_layer<S>(_config: &Config) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

/// The tracing side of the tracing-context layer, or `None` when off.
#[cfg(not(feature = "otlp-metrics"))]
pub fn context_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

/// The process metrics layer, or `None` when the feature is off.
#[cfg(feature = "procmetrics")]
pub fn proc_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    Some(Layer::boxed(ProcMetricsLayer::new()))
}

#[cfg(not(feature = "procmetrics"))]
pub fn proc_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

/// Install the recorder a process collector and a tracing-context layer need.
#[cfg(feature = "otlp-metrics")]
pub fn install(config: &Config) -> bool {
    match meter(config) {
        Some(meter) => install_recorder(meter),
        None => false,
    }
}

#[cfg(not(feature = "otlp-metrics"))]
pub fn install(_config: &Config) -> bool {
    false
}

#[cfg(feature = "sysmetrics")]
static OBSERVER: std::sync::Mutex<Option<Observer>> = std::sync::Mutex::new(None);

/// Start the bought process observer when the endpoint names a collector.
#[cfg(feature = "sysmetrics")]
pub fn start(config: &Config) -> bool {
    let Some(meter) = meter(config) else {
        return false;
    };
    let Some(observer) = start_observer(meter) else {
        return false;
    };
    if let Ok(mut slot) = OBSERVER.lock() {
        *slot = Some(observer);
    }
    true
}

#[cfg(feature = "sysmetrics")]
pub fn stop() {
    if let Ok(mut slot) = OBSERVER.lock() {
        if let Some(observer) = slot.take() {
            observer.finish();
        }
    }
}

#[cfg(not(feature = "sysmetrics"))]
pub fn start(_config: &Config) -> bool {
    false
}

#[cfg(not(feature = "sysmetrics"))]
pub fn stop() {}