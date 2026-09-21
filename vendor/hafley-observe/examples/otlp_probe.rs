//! OTLP probe: three nested spans exported to `HAFLEY_OTLP_ENDPOINT`.
//!
//! Point `HAFLEY_OTLP_ENDPOINT` at a local collector (for example
//! `http://127.0.0.1:4318/v1/traces`) to see `probe`, `parse` and `lower`
//! land in the store.

#[cfg(not(feature = "otlp-trace"))]
fn main() {}

#[cfg(feature = "otlp-trace")]
fn main() {
    hafley_observe::init(
        hafley_observe::Config::from_env(
            "observe-lab",
            env!("CARGO_PKG_VERSION"),
            "info",
            false,
        )
        .expect("log format"),
    )
    .expect("observability");

    let probe = tracing::info_span!("probe");
    let probe_guard = probe.enter();
    tracing::info!("probe start");
    {
        let parse = tracing::info_span!("parse", bytes = 4096u64);
        let _guard = parse.enter();
        tracing::info!(rules = 12, "parsed");
    }
    tracing::info!("between children");
    {
        let lower = tracing::info_span!("lower", rels = 7u64);
        let _guard = lower.enter();
        tracing::info!(tables = 3, "lowered");
    }
    tracing::info!("probe end");
    drop(probe_guard);
    drop(probe);

    hafley_observe::shutdown();
}
