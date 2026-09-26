//! Anti-cheat gates measured from timely/differential logs the test registers; the engine reports nothing itself.

mod support;

use differential_dataflow::logging::{DifferentialEvent, DifferentialEventBuilder};
use ivm_dd::{Agg, Dd, Engine, Frontier, LetRec, Op, Program, SourceChange, Stratum};
use ivm_dd::ReduceReadEventBuilder;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use timely::logging::{TimelyEvent, TimelyEventBuilder};

const PROGRAMS: [&str; 6] = ["0_access", "1_team_cost", "4_antijoin", "5_self_join", "6_topk", "7_reach"];

#[derive(Default)]
struct Seen {
    operates: Vec<String>,
    batch_rows: usize,
    reduce_reads: usize,
}

fn observed(program: &Program) -> (Dd, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (timely_seen, arrange_seen, reduce_seen) = (Arc::clone(&seen), Arc::clone(&seen), Arc::clone(&seen));
    let hook = Box::new(move |worker: &mut timely::worker::Worker| {
        let mut registry = worker.log_register().unwrap();
        registry.insert::<TimelyEventBuilder, _>("timely", move |_, data| {
            for (_, event) in data.iter().flat_map(|d| d.iter()) {
                if let TimelyEvent::Operates(op) = event {
                    timely_seen.lock().unwrap().operates.push(op.name.clone());
                }
            }
        });
        registry.insert::<DifferentialEventBuilder, _>("differential/arrange", move |_, data| {
            for (_, event) in data.iter().flat_map(|d| d.iter()) {
                if let DifferentialEvent::Batch(batch) = event {
                    arrange_seen.lock().unwrap().batch_rows += batch.length;
                }
            }
        });
        registry.insert::<ReduceReadEventBuilder, _>("lab/reduce_reads", move |_, data| {
            for (_, rows) in data.iter().flat_map(|d| d.iter()) {
                reduce_seen.lock().unwrap().reduce_reads += rows;
            }
        });
    });
    (Dd::install_observed(program, hook).unwrap(), seen)
}

fn insert(rel: u32, row: Vec<i64>) -> SourceChange {
    SourceChange { rel, row, w: 1 }
}

/// K9: the DD operators the dataflow logs are exactly those the `Op` list implies.
#[test]
fn k9_operator_census_matches_op_list() {
    for name in PROGRAMS {
        let program = support::program(name);
        let mut want: BTreeMap<&str, usize> = BTreeMap::new();
        for op in &program.nodes {
            match op {
                Op::Join { .. } => *want.entry("Join").or_default() += 1,
                Op::Antijoin { .. } => {
                    *want.entry("Join").or_default() += 1;
                    *want.entry("Threshold").or_default() += 1;
                }
                Op::Threshold(_) => *want.entry("Threshold").or_default() += 1,
                Op::Reduce { aggs, .. } => {
                    let extrema = aggs.iter().any(|a| matches!(a, Agg::Min(_) | Agg::Max(_)));
                    *want.entry("Reduce").or_default() += if extrema { 18 } else { 1 };
                    if extrema {
                        *want.entry("Threshold").or_default() += 2;
                        *want.entry("Join").or_default() += 1;
                    }
                }
                Op::TopK { .. } => *want.entry("Reduce").or_default() += 1,
                _ => {}
            }
        }
        for stratum in &program.strata {
            if let Stratum::LetRec(rec) = stratum {
                *want.entry("LetRec").or_default() += 1;
                *want.entry("Threshold").or_default() += rec.ids.len();
            }
        }
        let (mut dd, seen) = observed(&program);
        dd.settle(Frontier::default(), &mut ivm_dd::Raw::default()).unwrap();
        let mut got: BTreeMap<&str, usize> = BTreeMap::new();
        for op in &seen.lock().unwrap().operates {
            if let Some(k) = ["Join", "Threshold", "Reduce", "LetRec"].into_iter().find(|k| op == k) {
                *got.entry(k).or_default() += 1;
            }
        }
        assert_eq!(got, want, "{name}: operator census");
    }
}

/// K2: every operator exists after install; settles build nothing.
#[test]
fn k2_no_operator_created_after_install() {
    for name in PROGRAMS {
        let program = support::program(name);
        let (mut dd, seen) = observed(&program);
        dd.settle(Frontier::default(), &mut ivm_dd::Raw::default()).unwrap();
        let installed = seen.lock().unwrap().operates.len();
        let source = program.rels.iter().find(|r| r.kind == ivm_dd::RelKind::Source).unwrap();
        for i in 0..5 {
            let row = vec![i; source.cols.len()];
            dd.settle(Frontier { changes: vec![insert(source.id, row)] }, &mut ivm_dd::Raw::default()).unwrap();
        }
        assert_eq!(seen.lock().unwrap().operates.len(), installed, "{name}: operators created during settle");
    }
}

/// K1: rows arranged by a one-row change do not grow with the rows already loaded.
#[test]
fn k1_one_row_change_work_is_independent_of_loaded_size() {
    let program = support::program("0_access");
    let arranged_by_one_change = |loaded: i64| -> usize {
        let (mut dd, seen) = observed(&program);
        let mut load = vec![insert(1, vec![10, 100])];
        load.extend((0..loaded).map(|person| insert(0, vec![person, 10])));
        dd.settle(Frontier { changes: load }, &mut ivm_dd::Raw::default()).unwrap();
        seen.lock().unwrap().batch_rows = 0;
        dd.settle(Frontier { changes: vec![insert(2, vec![-1, 7])] }, &mut ivm_dd::Raw::default()).unwrap();
        let rows = seen.lock().unwrap().batch_rows;
        rows
    };
    let small = arranged_by_one_change(1_000);
    let large = arranged_by_one_change(30_000);
    assert!(small > 0, "logger saw no batches");
    assert!(large <= small * 2, "one-row change arranged {small} rows at 1e3 loaded, {large} at 3e4");
}

/// K1: count arrangement records actually read by every hierarchical reduce closure.
/// The logger fires for each affected bucket, including buckets whose extrema do not change.
#[test]
fn k1_team_cost_reduce_reads_grow_logarithmically() {
    let program = support::program("1_team_cost");
    let work = |group_size: i64| -> (usize, usize) {
        let (mut dd, seen) = observed(&program);
        let load = (0..group_size).map(|id| insert(0, vec![id, 0, id + 1])).collect();
        dd.settle(Frontier { changes: load }, &mut ivm_dd::Raw::default()).unwrap();
        {
            let mut seen = seen.lock().unwrap();
            seen.batch_rows = 0;
            seen.reduce_reads = 0;
        }
        dd.settle(Frontier { changes: vec![insert(0, vec![-1, 0, 0])] }, &mut ivm_dd::Raw::default()).unwrap();
        let seen = seen.lock().unwrap();
        (seen.reduce_reads, seen.batch_rows)
    };
    let small = work(100);
    let large = work(10_000);
    println!("team_cost K1 group=100 reads={} batch_rows={}; group=10000 reads={} batch_rows={}", small.0, small.1, large.0, large.1);
    assert!(small.0 > 0 && small.1 > 0, "loggers saw no reduce reads or arrangement batches");
    assert!(large.0 <= 16 * 15, "10k-row group read {} arrangement records", large.0);
    assert!(large.0 <= small.0 * 2, "reduce reads at group sizes 100 and 10000: {small:?}, {large:?}");
}

#[test]
fn hierarchical_reduce_inside_letrec_matches_top_level() {
    let plain = support::program("1_team_cost");
    let mut nested = plain.clone();
    nested.strata[0] = Stratum::LetRec(LetRec { ids: vec![1], bodies: vec![1], limit: None });
    let mut top = Dd::install(&plain).unwrap();
    let mut inner = Dd::install(&nested).unwrap();
    let steps = [
        Frontier { changes: vec![insert(0, vec![1, 0, 4]), insert(0, vec![2, 0, 9]), insert(0, vec![3, 1, 6])] },
        Frontier { changes: vec![insert(0, vec![4, 0, 2])] },
        Frontier { changes: vec![SourceChange { rel: 0, row: vec![1, 0, 4], w: -1 }] },
    ];
    for frontier in steps {
        assert_eq!(inner.settle(frontier.clone(), &mut ivm_dd::Raw::default()).unwrap(), top.settle(frontier, &mut ivm_dd::Raw::default()).unwrap());
        assert_eq!(inner.snapshot(1, &mut ivm_dd::Raw::default()).unwrap(), top.snapshot(1, &mut ivm_dd::Raw::default()).unwrap());
    }
}

#[test]
fn hierarchical_reduce_preserves_live_rows_with_signed_inputs() {
    let program: Program = serde_json::from_str(r#"{
        "rels": [
            {"id": 0, "name": "positive", "cols": ["Int", "Int", "Int"], "kind": "Source"},
            {"id": 1, "name": "negative", "cols": ["Int", "Int", "Int"], "kind": "Source"},
            {"id": 2, "name": "mixed", "cols": ["Int", "Int", "Int", "Int", "Int"], "kind": "Derived"},
            {"id": 3, "name": "extrema", "cols": ["Int", "Int"], "kind": "Derived"}
        ],
        "nodes": [
            {"Get": 0}, {"Get": 1}, {"Negate": 1}, {"Union": [0, 2]},
            {"Reduce": {"input": 3, "key": [1], "aggs": ["Count", {"Sum": 2}, {"Min": 2}, {"Max": 2}]}},
            {"Reduce": {"input": 3, "key": [1], "aggs": [{"Min": 2}]}}
        ],
        "strata": [{"Let": {"id": 2, "body": 4}}, {"Let": {"id": 3, "body": 5}}],
        "outputs": [2, 3]
    }"#).unwrap();
    let mut dd = Dd::install(&program).unwrap();
    dd.settle(Frontier { changes: vec![
        insert(0, vec![1, 0, 5]),
        insert(0, vec![3, 0, 10]),
        insert(1, vec![2, 0, 5]),
    ] }, &mut ivm_dd::Raw::default()).unwrap();
    assert_eq!(dd.snapshot(2, &mut ivm_dd::Raw::default()).unwrap(), vec![(vec![0, 1, 10, 5, 10], 1)]);
    assert_eq!(dd.snapshot(3, &mut ivm_dd::Raw::default()).unwrap(), vec![(vec![0, 5], 1)]);
}

/// K20: two engines with different programs, settles interleaved, each equals its solo run.
#[test]
fn k20_two_engines_interleaved_match_solo_runs() {
    let access = support::program("0_access");
    let reach = support::program("7_reach");
    let a_steps: Vec<Frontier> = (0..4).map(|i| Frontier { changes: vec![insert(0, vec![i, 10]), insert(1, vec![10, 100 + i])] }).collect();
    let r_steps: Vec<Frontier> = (0..4).map(|i| Frontier { changes: vec![insert(0, vec![i, i + 1])] }).collect();
    let solo = |program: &Program, steps: &[Frontier]| -> Vec<Vec<(u32, Vec<i64>, i64)>> {
        let mut dd = Dd::install(program).unwrap();
        steps.iter().map(|f| dd.settle(f.clone(), &mut ivm_dd::Raw::default()).unwrap().changes).collect()
    };
    let (want_a, want_r) = (solo(&access, &a_steps), solo(&reach, &r_steps));
    let (mut a, mut r) = (Dd::install(&access).unwrap(), Dd::install(&reach).unwrap());
    for i in 0..4 {
        assert_eq!(a.settle(a_steps[i].clone(), &mut ivm_dd::Raw::default()).unwrap().changes, want_a[i], "access step {i}");
        assert_eq!(r.settle(r_steps[i].clone(), &mut ivm_dd::Raw::default()).unwrap().changes, want_r[i], "reach step {i}");
    }
}

/// K6, K20, K27: engine sources read no files, hold no global state, name no test relation; only the SQLite engine links SQLite.
#[test]
fn k6_dd_engine_source_gate() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden = [
        "include_str!", "include_bytes!", "std::fs", ".tsv", "plans/", "\nstatic ", "\npub static ", "thread_local!", "OnceLock",
        "lazy_static", "rusqlite", "Connection", "\"membership\"", "\"permission\"", "\"direct_grant\"", "\"job\"",
        "\"access\"", "\"team_cost\"",
    ];
    for (file, path) in [
        ("0_ir.rs", "../ivm-ir/src/0_ir.rs"),
        ("1_rel.rs", "../ivm-engine/src/1_rel.rs"),
        ("2_dd.rs", "src/2_dd.rs"),
        ("3_sqlite.rs", "../../labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow/src/3_sqlite.rs"),
        ("lib.rs", "src/lib.rs"),
    ] {
        let text = std::fs::read_to_string(dir.join(path)).unwrap();
        let text: String = text.lines().filter(|l| !l.trim_start().starts_with("//") && !l.contains("cfg(feature")).map(str::trim_start).collect::<Vec<_>>().join("\n");
        for token in forbidden {
            let links_sqlite = file == "lib.rs" || file == "3_sqlite.rs";
            let allowed = links_sqlite && (token == "rusqlite" || token == "Connection");
            assert!(allowed || !text.contains(token), "{file} contains {token:?}");
        }
    }
}
