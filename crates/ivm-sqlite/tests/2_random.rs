//! Generated programs against the SQLite recompute oracle and K5 value transform.

#[path = "../../ivm-dd/tests/random/mod.rs"]
mod random;

use ivm_sqlite::Sqlite;
use ivm_dd::Dd;
use random::{drive, meta, run};
use drive::Case;
use std::process::Command;

fn any_oracle<E: ivm_dd::Engine>(seed: u64) {
    use ivm_dd::{AnyValue, Frontier, RelKind, SourceChange, Ty};
    use rusqlite::{types::Value, Connection};

    let mut rng = random::rng::Rng(seed);
    let program = random::gen::any_program();
    let values = random::gen::any_values(&mut rng);
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch(&random::sql::ddl(&program)).unwrap();
    let mut engine = E::install(&program).unwrap();
    let mut changes = Vec::new();
    for (i, value) in values.iter().enumerate() {
        oracle.execute_batch(&format!("INSERT INTO mixed_source VALUES ({}, {})", random::sql::any_literal(value), i + 1)).unwrap();
        changes.push(SourceChange { rel: 0, row: vec![engine.intern_any(value).unwrap(), i as i64 + 1], w: 1 });
    }
    engine.settle(Frontier { changes }).unwrap();
    for rel in program.rels.iter().filter(|rel| rel.kind == RelKind::Derived) {
        let mut stmt = oracle.prepare(&format!("SELECT * FROM {}", rel.name)).unwrap();
        let width = stmt.column_count();
        let mut want = stmt.query_map([], |row| (0..width).map(|i| row.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>())
            .unwrap().map(Result::unwrap).collect::<Vec<_>>();
        let mut got = engine.snapshot(rel.id).unwrap().into_iter().flat_map(|(row, weight)| {
            let row = row.iter().zip(&rel.cols).map(|(cell, ty)| match ty {
                Ty::Any => match engine.any_value(*cell).unwrap() {
                    AnyValue::Null => Value::Null,
                    AnyValue::Integer(v) => Value::Integer(v),
                    AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
                    AnyValue::Text(v) => Value::Text(v),
                    AnyValue::Blob(v) => Value::Blob(v),
                },
                Ty::Int => Value::Integer(*cell),
                _ => panic!("unexpected random output type"),
            }).collect::<Vec<_>>();
            std::iter::repeat_n(row, weight as usize)
        }).collect::<Vec<_>>();
        let sort = |rows: &mut Vec<Vec<Value>>| rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        sort(&mut want);
        sort(&mut got);
        assert_eq!(got, want, "seed {seed} relation {}", rel.name);
    }
}

#[test]
fn random_any_sqlite_oracle() {
    isolated("random_any_sqlite_oracle_worker");
}

#[test]
#[ignore = "run by random_any_sqlite_oracle in a separate process"]
fn random_any_sqlite_oracle_worker() {
    for seed in random::drive::seeds(1000) {
        any_oracle::<Dd>(seed);
        any_oracle::<Sqlite>(seed);
    }
}

#[test]
fn captured_oracle_ir_agrees_with_dd() {
    // Lowered from sprefa oracle/eval on 2026-09-27. The corpus holds the dd frontiers; the sqlite
    // frontiers are the same rows with each term id translated to the sqlite engine's id.
    for (name, source) in [
        ("3_count", include_str!("corpus/9_3_count_case.json")),
        ("16_intern_row_reuse", include_str!("corpus/10_16_intern_row_reuse_case.json")),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(source).unwrap();
        let program: ivm_ir::Program = serde_json::from_value(fixture["program"].clone()).unwrap();
        let texts: Vec<String> = serde_json::from_value(fixture["texts"].clone()).unwrap();
        let dd_frontiers: Vec<ivm_ir::Frontier> = serde_json::from_value(fixture["dd_frontiers"].clone()).unwrap();
        drive::agreement_frontiers::<Dd, Sqlite>(&program, &texts, &dd_frontiers)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

fn isolated(worker: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", worker, "--nocapture"])
        .status()
        .unwrap();
    assert!(status.success(), "{worker} exited with {status}");
}

#[test]
fn random_sql() {
    isolated("random_sql_worker");
}

#[test]
#[ignore = "run by random_sql in a separate process"]
fn random_sql_worker() {
    run("random_sql", 200, Case::generate, drive::oracle::<Sqlite>);
}

#[test]
fn random_typed_dd_sql() {
    isolated("random_typed_dd_sql_worker");
}

#[test]
#[ignore = "run by random_typed_dd_sql in a separate process"]
fn random_typed_dd_sql_worker() {
    run("random_typed_dd_sql", 1000, Case::generate_typed, drive::agreement::<Dd, Sqlite>);
}

#[test]
fn random_mint_dd_sql() {
    isolated("random_mint_dd_sql_worker");
}

#[test]
#[ignore = "run by random_mint_dd_sql in a separate process"]
fn random_mint_dd_sql_worker() {
    for seed in drive::seeds(200) {
        let case = Case::generate_mint(seed);
        drive::agreement::<Dd, Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        drive::term_lt_structure::<Dd>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        drive::term_lt_structure::<Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn random_recursive_shapes_dd_sql() {
    isolated("random_recursive_shapes_dd_sql_worker");
}

#[test]
#[ignore = "run by random_recursive_shapes_dd_sql in a separate process"]
fn random_recursive_shapes_dd_sql_worker() {
    for seed in drive::seeds(200) {
        let case = Case::generate_recursive_shapes(seed);
        drive::agreement::<Dd, Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn k5_sql() {
    isolated("k5_sql_worker");
}

#[test]
#[ignore = "run by k5_sql in a separate process"]
fn k5_sql_worker() {
    run("k5_sql", 100, Case::generate_k5, meta::values::<Sqlite>);
}

#[test]
fn k5_oracle_sql() {
    isolated("k5_oracle_sql_worker");
}

#[test]
#[ignore = "run by k5_oracle_sql in a separate process"]
fn k5_oracle_sql_worker() {
    run("k5_oracle_sql", 100, Case::generate_k5, drive::oracle::<Sqlite>);
}
