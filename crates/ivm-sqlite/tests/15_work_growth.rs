//! Work of a generated program family at sizes n and 4n for n = 4, 16, 64 (rules, relations and nodes all scale with
//! n): every pinned count grows Linear or slower on both engines. SQLite counts are read from the
//! `ivm_sqlite::work` events through `oh::CountRecorder`; DD counts from `counters()`.
#![cfg(not(feature = "image"))]

extern crate hafley_observe as oh;

use oh::{assert_growth_at_most, growth_between, CountRecorder, Growth};
use ivm_dd::Dd;
use ivm_engine::{Counters, Engine};
use ivm_ir::{Frontier, LetRec, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use std::collections::BTreeMap;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, Layer as _};

oh::counting_allocator!();

/// Small sizes n; each runs against 4n.
const SIZES: [usize; 3] = [4, 16, 64];
const RATIO: usize = 4;
const EDGES: i64 = 8;

/// `n` reach rules over one edge source: n + 1 relations, n LetRec strata, 5n nodes.
fn family(n: usize) -> Program {
    let mut program = Program {
        terms: vec![],
        texts: vec![],
        rels: vec![Relation { id: 0, name: "edges".into(), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Source }],
        nodes: vec![],
        strata: vec![],
        outputs: (1..=n as u32).collect(),
    };
    for id in 1..=n as u32 {
        program.rels.push(Relation { id, name: format!("reach_{id}"), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Derived });
        let at = program.nodes.len() as u32;
        program.nodes.extend([
            Op::Get(0),
            Op::Get(id),
            Op::Join { inputs: vec![at + 1, at], equivalences: vec![vec![(0, 1), (1, 0)]] },
            Op::Mfp { input: at + 2, filter: vec![], map: vec![], project: vec![0, 3] },
            Op::Union(vec![at, at + 3]),
        ]);
        program.strata.push(Stratum::LetRec(LetRec { ids: vec![id], bodies: vec![at + 4], limit: None, nested: vec![] }));
    }
    program
}

fn chain() -> Frontier {
    Frontier { changes: (0..EDGES).map(|i| SourceChange { rel: 0, row: vec![i, i + 1], w: 1 }).collect() }
}

/// Field sums of the `ivm_sqlite::work` events per phase, for one install and one settle.
fn sqlite_work(n: usize) -> BTreeMap<String, BTreeMap<String, u64>> {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry()
        .with(layer.with_filter(Targets::new().with_target("ivm_sqlite::work", tracing::Level::INFO)));
    tracing::subscriber::with_default(subscriber, || {
        let mut engine = Sqlite::install(&family(n)).unwrap();
        engine.settle(chain()).unwrap();
    });
    recorder
        .event_sums("ivm_sqlite::work", tracing::Level::INFO, "", "", Some("phase"))
        .into_iter()
        .map(|((_, phase), sums)| (phase, sums.sums.into_iter().map(|(field, sum)| (field, sum as u64)).collect()))
        .collect()
}

fn dd_work(n: usize) -> Counters {
    let mut engine = Dd::install(&family(n)).unwrap();
    engine.settle(chain()).unwrap();
    engine.counters()
}

fn dd_counts(counters: Counters) -> [(&'static str, u64); 5] {
    let delta = counters.delta_rows;
    let delta_rows = [delta.filter, delta.join, delta.antijoin, delta.reduce, delta.topk, delta.window, delta.mint]
        .into_iter().flatten().sum();
    [
        ("rows_written", counters.rows_written),
        ("delta_rows", delta_rows),
        ("delta_rows_join", delta.join.unwrap_or(0)),
        ("rounds", counters.rounds.unwrap_or(0)),
        ("interned", counters.interned.unwrap_or(0)),
    ]
}

/// Every count with its class at n -> 4n: `sqlite <phase> <field>` and `dd settle <counter>`.
fn rows(small: usize) -> Vec<(String, u64, u64)> {
    let large = small * RATIO;
    let (small_sql, large_sql) = (sqlite_work(small), sqlite_work(large));
    let mut rows = Vec::new();
    for phase in ["install", "settle"] {
        let (before, after) = (&small_sql[phase], &large_sql[phase]);
        let mut fields: BTreeMap<String, (u64, u64)> = before.keys().chain(after.keys())
            .map(|field| (field.clone(), (before.get(field).copied().unwrap_or(0), after.get(field).copied().unwrap_or(0))))
            .collect();
        let written = fields.iter().filter(|(field, _)| field.starts_with("written_"))
            .fold((0, 0), |(a, b), (_, (x, y))| (a + x, b + y));
        fields.insert("written".into(), written);
        rows.extend(fields.into_iter().map(|(field, (a, b))| (format!("sqlite {phase} {field}"), a, b)));
    }
    let (small_dd, large_dd) = (dd_counts(dd_work(small)), dd_counts(dd_work(large)));
    rows.extend(small_dd.into_iter().zip(large_dd).map(|((name, a), (_, b))| (format!("dd settle {name}"), a, b)));
    rows
}

#[oh::test(memory_bytes = 268435456)]
fn work_grows_linear_or_slower() {
    for small in SIZES {
        let rows = rows(small);
        for (name, a, b) in &rows {
            eprintln!("n={small} {name} {a} {b} {:?}", growth_between(*a, *b, RATIO as f64));
        }
        for name in [
            "sqlite install creates",
            "sqlite install schema_rows_parsed",
            "sqlite install prepared_bytes",
            "sqlite settle prepared_bytes",
            "sqlite settle vm_steps",
            "sqlite settle written",
            "dd settle rows_written",
            "dd settle delta_rows",
            "dd settle rounds",
        ] {
            let (_, a, b) = rows.iter().find(|(row, _, _)| row == name).unwrap_or_else(|| panic!("{name} missing"));
            assert_growth_at_most(&format!("n={small} {name}"), *a, *b, RATIO as f64, Growth::Linear);
        }
    }
}
