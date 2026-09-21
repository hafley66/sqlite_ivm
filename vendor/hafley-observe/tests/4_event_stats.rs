use hafley_observe::{CountRecorder, FieldStats};
use tracing_subscriber::prelude::*;

#[test]
fn grouped_samples_preserve_missing_fields_final_fields_and_nearest_ancestor() {
    let (recorder, layer) = CountRecorder::new();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        tracing::debug!(target: "samples", elapsed = 999u64);
        let outer = tracing::debug_span!("operation", kind = "outer", rows = 99u64);
        let _outer = outer.enter();
        {
            let inner =
                tracing::debug_span!("operation", kind = "inner", rows = tracing::field::Empty);
            let _inner = inner.enter();
            tracing::debug!(target: "samples", elapsed = 10u64, delta = -2i64);
            tracing::debug!(target: "samples", elapsed = 30u64, fraction = 0.5f64);
            tracing::info!(target: "samples", elapsed = 1000u64);
            tracing::debug!(target: "other", elapsed = 1000u64);
            inner.record("rows", 4u64);
        }
        tracing::debug!(target: "samples", elapsed = 5u64);
    });
    let groups = recorder.event_stats(
        "samples",
        tracing::Level::DEBUG,
        "operation",
        ["kind", "absent"],
    );
    assert_eq!(groups.len(), 2);
    let inner = &groups[&["inner".to_owned(), String::new()]];
    assert_eq!(inner.events, 2);
    assert_eq!(inner.fields["elapsed"].samples, [10.0, 30.0]);
    assert_eq!(inner.fields["delta"].samples, [-2.0]);
    assert_eq!(inner.fields["fraction"].samples, [0.5]);
    assert_eq!(inner.ancestor_fields["rows"].samples, [4.0, 4.0]);
    let outer = &groups[&["outer".to_owned(), String::new()]];
    assert_eq!(outer.events, 1);
    assert_eq!(outer.fields["elapsed"].samples, [5.0]);
}

#[test]
fn nearest_rank_percentiles_cover_empty_singleton_and_tail() {
    let empty = FieldStats::default();
    assert_eq!(
        (empty.sum(), empty.mean(), empty.percentile(99.0)),
        (0.0, None, None)
    );
    let singleton = FieldStats { samples: vec![7.0] };
    assert_eq!(
        (singleton.mean(), singleton.percentile(99.0)),
        (Some(7.0), Some(7.0))
    );
    let series = FieldStats {
        samples: (1..=100).rev().map(f64::from).collect(),
    };
    assert_eq!(series.sum(), 5050.0);
    assert_eq!(series.mean(), Some(50.5));
    assert_eq!(
        [0.0, 50.0, 99.0, 100.0].map(|p| series.percentile(p)),
        [Some(1.0), Some(50.0), Some(99.0), Some(100.0)]
    );
}
