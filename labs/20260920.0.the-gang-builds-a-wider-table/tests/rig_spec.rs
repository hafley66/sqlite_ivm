use lab_20260920_0::observe::{self, assert_growth, CountRecorder, Growth};
use lab_20260920_0::rig::{self, Axes, FoldReport, PrepareMode, CHANGE_SPAN, FOLD_SPAN, STEP_GUARD_PRAGMA, STEP_GUARD_TYPES, STEP_NAMES, STEP_UPSERT};
use rusqlite::Connection;

fn base_axes(seed: u64) -> Axes {
    Axes {
        tables: 4,
        columns: 10,
        join_arity: 2,
        seed,
    }
}

fn run_recordered(axes: &Axes, changes: usize) -> (CountRecorder, FoldReport) {
    observe::pin_log_filter();
    let tables = rig::generate(axes).expect("generate");
    let db = Connection::open_in_memory().expect("open");
    rig::install(&db, axes, &tables).expect("install");
    let plan = rig::fold_plan(axes, changes).expect("plan");
    let (recorder, layer) = CountRecorder::new();
    let _scope = observe::scoped_subscriber(layer);
    let report = rig::run_fold(&db, axes, &plan, PrepareMode::Engine).expect("fold");
    (recorder, report)
}

// Two runs at the same seed must replay byte-identical fixtures and plans, or
// two labs measuring the same axes are not measuring the same thing.
#[test]
fn same_seed_same_tables() {
    let first = rig::generate(&base_axes(42)).expect("generate");
    let second = rig::generate(&base_axes(42)).expect("generate");
    assert_eq!(first, second);
    let first_plan = rig::fold_plan(&base_axes(42), 32).expect("plan");
    let second_plan = rig::fold_plan(&base_axes(42), 32).expect("plan");
    assert_eq!(first_plan, second_plan);
}

// A generator that ignores its seed looks reproducible and is useless.
#[test]
fn different_seed_different_tables() {
    let first = rig::generate(&base_axes(42)).expect("generate");
    let second = rig::generate(&base_axes(43)).expect("generate");
    assert_ne!(first, second);
    let first_plan = rig::fold_plan(&base_axes(42), 32).expect("plan");
    let second_plan = rig::fold_plan(&base_axes(43), 32).expect("plan");
    assert_ne!(first_plan, second_plan);
}

// Width is the point of the rig: the fixture, the schema and pragma_table_info
// must all agree on M for the narrow and the wide setting.
#[test]
fn column_count_is_honored() {
    for columns in [10usize, 20] {
        let axes = Axes {
            columns,
            ..base_axes(42)
        };
        let tables = rig::generate(&axes).expect("generate");
        assert!(tables
            .iter()
            .all(|table| table.rows.iter().all(|row| row.len() == columns)));
        let db = Connection::open_in_memory().expect("open");
        let schema = rig::install(&db, &axes, &tables).expect("install");
        assert_eq!(schema.columns, columns);
        let declared: i64 = db
            .query_row(
                &format!(
                    "SELECT count(*) FROM pragma_table_info('{}')",
                    tables[0].name
                ),
                [],
                |row| row.get(0),
            )
            .expect("table_info");
        assert_eq!(declared as usize, columns);
    }
}

// J is the other axis: the view must join exactly that many tables and the
// fold must run over it without error.
#[test]
fn join_arity_is_honored() {
    for arity in [2usize, 3, 4] {
        let axes = Axes {
            tables: 8,
            join_arity: arity,
            ..base_axes(42)
        };
        let tables = rig::generate(&axes).expect("generate");
        let db = Connection::open_in_memory().expect("open");
        let schema = rig::install(&db, &axes, &tables).expect("install");
        assert_eq!(schema.view_tables.len(), arity);
        for index in 0..arity {
            assert!(
                schema.view_query_sql.contains(&format!("t{index}")),
                "view joins t{index}"
            );
        }
        assert!(!schema.view_query_sql.contains(&format!("t{arity}")));
        let plan = rig::fold_plan(&axes, 16).expect("plan");
        rig::run_fold(&db, &axes, &plan, PrepareMode::Engine).expect("fold");
    }
}

// The stable span names are the contract later labs key on. Every step must
// resolve in the recorder, fire once per change under the change span, and
// grow linearly when the fold depth doubles.
#[test]
fn spans_are_countable_by_the_recorder() {
    let axes = base_axes(42);
    let (recorder, report) = run_recordered(&axes, 32);
    let counts = recorder.counts();
    let changes = report.changes;
    for name in STEP_NAMES {
        assert_eq!(
            counts.entries_of(name),
            changes,
            "step {name} fires once per change"
        );
        counts.assert_children_at_most(CHANGE_SPAN, name, changes);
    }
    counts.assert_instances(CHANGE_SPAN, changes);
    counts.assert_instances(FOLD_SPAN, 1);
    let (large_recorder, _) = run_recordered(&axes, 64);
    let large = large_recorder.counts();
    assert_growth(&counts, &large, STEP_GUARD_TYPES, 2.0, Growth::Linear);
    assert_growth(&counts, &large, STEP_UPSERT, 2.0, Growth::Linear);
}

// The rig pins the log filter itself; an ambient HAFLEY_LOG must not survive
// the pin, because upstream falls back to trace and the logger would become
// the thing being measured.
#[test]
fn filter_is_pinned_never_inherited() {
    std::env::set_var("HAFLEY_LOG", "trace");
    observe::pin_log_filter();
    assert_eq!(
        std::env::var("HAFLEY_LOG").as_deref(),
        Ok(observe::PINNED_LOG)
    );
    assert_ne!(observe::PINNED_LOG, "trace");
}

// Budget ceilings fail with a diagnostic that names the constant protecting
// them, so a runaway axis is a named budget breach, not a mystery OOM.
#[test]
fn axis_budgets_name_their_ceiling() {
    let too_few_tables = rig::generate(&Axes {
        tables: 1,
        ..base_axes(42)
    })
    .expect_err("tables below floor");
    assert!(too_few_tables.0.contains("MAX_TABLES"), "{too_few_tables}");
    let too_wide = rig::generate(&Axes {
        columns: 65,
        ..base_axes(42)
    })
    .expect_err("columns over ceiling");
    assert!(too_wide.0.contains("MAX_COLUMNS"), "{too_wide}");
    let too_deep = rig::fold_plan(&base_axes(42), rig::MAX_CHANGES_PER_FOLD + 1)
        .expect_err("changes over ceiling");
    assert!(too_deep.0.contains("MAX_CHANGES_PER_FOLD"), "{too_deep}");
}
