use std::time::Instant;

use lab_20260920_0::observe::{self, CountRecorder};
use lab_20260920_0::rig::{self, Axes, PrepareMode, CHANGE_SPAN, FOLD_SPAN, STEP_NAMES};
use rusqlite::Connection;

fn main() {
    observe::pin_log_filter();
    let seed = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(42);
    let axes = Axes {
        tables: 8,
        columns: 20,
        join_arity: 4,
        seed,
    };
    let tables = rig::generate(&axes).expect("generate");
    let db = Connection::open_in_memory().expect("open");
    let schema = rig::install(&db, &axes, &tables).expect("install");
    let plan = rig::fold_plan(&axes, rig::CHANGES_PER_FOLD).expect("plan");
    let (recorder, layer) = CountRecorder::new();
    let _scope = observe::scoped_subscriber(layer);
    let started = Instant::now();
    let report = rig::run_fold(&db, &axes, &plan, PrepareMode::Engine).expect("fold");
    let wall = started.elapsed();
    let counts = recorder.counts();
    let timings = recorder.timings();

    println!(
        "fold: {} changes, tables={} columns={} join_arity={} seed={}",
        report.changes, axes.tables, axes.columns, axes.join_arity, axes.seed
    );
    println!(
        "view: {} state: {} joined: {:?}",
        schema.view, schema.state, schema.view_tables
    );
    println!(
        "wall: {wall:.3?}, log filter pinned to {:?}",
        observe::PINNED_LOG
    );
    println!("{:<22} {:>10} {:>10} {:>14}", "span", "instances", "entries", "nanos");
    for name in STEP_NAMES.into_iter().chain([FOLD_SPAN, CHANGE_SPAN]) {
        println!(
            "{:<22} {:>10} {:>10} {:>14}",
            name,
            counts.instances_of(name),
            counts.entries_of(name),
            timings.nanos.get(name).copied().unwrap_or_default()
        );
    }
}
