use hafley_observe::{assert_growth, CountRecorder, Growth};
use tracing_subscriber::prelude::*;

fn drive_batched(rows: usize) {
    let populate = tracing::info_span!("populate", rows);
    let _entered = populate.enter();
    let batch = tracing::info_span!("maintain_batch");
    let _batch = batch.enter();
}

fn drive_per_row(rows: usize) {
    let populate = tracing::info_span!("populate", rows);
    let _entered = populate.enter();
    for row in 0..rows {
        let maintain = tracing::info_span!("maintain", row);
        let _maintain = maintain.enter();
    }
}

fn counts_for(rows: usize, drive: fn(usize)) -> hafley_observe::SpanCounts {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || drive(rows));
    recorder.counts()
}

#[test]
fn per_row_maintenance_reads_as_linear_fanout() {
    let small = counts_for(100, drive_per_row);
    let large = counts_for(200, drive_per_row);

    small.assert_instances("populate", 1);
    assert_eq!(small.children_of("populate", "maintain"), 100);
    assert_eq!(large.children_of("populate", "maintain"), 200);

    assert_growth(&small, &large, "maintain", 2.0, Growth::Linear);
    assert_growth(&small, &large, "populate", 2.0, Growth::Constant);
}

#[test]
fn batched_maintenance_reads_as_constant_fanout() {
    let small = counts_for(100, drive_batched);
    let large = counts_for(200, drive_batched);

    small.assert_children_at_most("populate", "maintain_batch", 1);
    assert_growth(&small, &large, "maintain_batch", 2.0, Growth::Constant);
}
