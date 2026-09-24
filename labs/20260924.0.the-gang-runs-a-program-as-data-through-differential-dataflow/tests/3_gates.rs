//! Anti-cheat gates measured from timely/differential logs the test registers; the engine reports nothing itself.

mod support;

use differential_dataflow::logging::{DifferentialEvent, DifferentialEventBuilder};
use lab_20260924_0::{Dd, Engine, Frontier, Op, Program, SourceChange, Stratum};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use timely::logging::{TimelyEvent, TimelyEventBuilder};

const PROGRAMS: [&str; 6] = ["0_access", "1_team_cost", "4_antijoin", "5_self_join", "6_topk", "7_reach"];

#[derive(Default)]
struct Seen {
    operates: Vec<String>,
    batch_rows: usize,
}

fn observed(program: &Program) -> (Dd, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (timely_seen, arrange_seen) = (Arc::clone(&seen), Arc::clone(&seen));
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
                Op::Reduce { .. } | Op::TopK { .. } => *want.entry("Reduce").or_default() += 1,
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
        dd.settle(Frontier::default()).unwrap();
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
        dd.settle(Frontier::default()).unwrap();
        let installed = seen.lock().unwrap().operates.len();
        let source = program.rels.iter().find(|r| r.kind == lab_20260924_0::RelKind::Source).unwrap();
        for i in 0..5 {
            let row = vec![i; source.cols.len()];
            dd.settle(Frontier { changes: vec![insert(source.id, row)] }).unwrap();
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
        dd.settle(Frontier { changes: load }).unwrap();
        seen.lock().unwrap().batch_rows = 0;
        dd.settle(Frontier { changes: vec![insert(2, vec![-1, 7])] }).unwrap();
        let rows = seen.lock().unwrap().batch_rows;
        rows
    };
    let small = arranged_by_one_change(1_000);
    let large = arranged_by_one_change(30_000);
    assert!(small > 0, "logger saw no batches");
    assert!(large <= small * 2, "one-row change arranged {small} rows at 1e3 loaded, {large} at 3e4");
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
        steps.iter().map(|f| dd.settle(f.clone()).unwrap().changes).collect()
    };
    let (want_a, want_r) = (solo(&access, &a_steps), solo(&reach, &r_steps));
    let (mut a, mut r) = (Dd::install(&access).unwrap(), Dd::install(&reach).unwrap());
    for i in 0..4 {
        assert_eq!(a.settle(a_steps[i].clone()).unwrap().changes, want_a[i], "access step {i}");
        assert_eq!(r.settle(r_steps[i].clone()).unwrap().changes, want_r[i], "reach step {i}");
    }
}

/// K6, K20, K27: the DD engine source reads no files, holds no global state, links no SQLite, names no test relation.
#[test]
fn k6_dd_engine_source_gate() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "include_str!", "include_bytes!", "std::fs", ".tsv", "plans/", "\nstatic ", "\npub static ", "thread_local!", "OnceLock",
        "lazy_static", "rusqlite", "Connection", "\"membership\"", "\"permission\"", "\"direct_grant\"", "\"job\"",
        "\"access\"", "\"team_cost\"",
    ];
    for file in ["0_ir.rs", "1_rel.rs", "2_dd.rs", "lib.rs"] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        let text: String = text.lines().filter(|l| !l.trim_start().starts_with("//") && !l.contains("cfg(feature")).map(str::trim_start).collect::<Vec<_>>().join("\n");
        for token in forbidden {
            let allowed = file == "lib.rs" && (token == "rusqlite" || token == "Connection");
            assert!(allowed || !text.contains(token), "{file} contains {token:?}");
        }
    }
}
