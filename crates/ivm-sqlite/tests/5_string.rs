use ivm_dd::Dd;
use ivm_engine::{Engine, Raw};
use ivm_ir::{Delta, Frontier, Program, SourceChange};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;
use std::collections::BTreeMap;

#[path = "../../ivm-dd/tests/random/0_rng.rs"]
#[allow(dead_code)]
mod rng;

fn rows<E: Engine>(engine: &E, db: &Connection, raw: Vec<(Vec<i64>, i64)>) -> Vec<(Vec<String>, i64)> {
    let mut out = raw.into_iter().map(|(row, w)| {
        let row = row.into_iter().map(|id| engine.text(id, &mut Raw::with_connection(db)).unwrap().unwrap()).collect();
        (row, w)
    }).collect::<Vec<_>>();
    out.sort();
    out
}

fn oracle_rows(db: &Connection, name: &str) -> Vec<(Vec<String>, i64)> {
    let mut stmt = db.prepare(&format!("SELECT * FROM {name}")).unwrap();
    let width = stmt.column_count();
    let raw = stmt.query_map([], |r| {
        (0..width).map(|i| r.get::<_, String>(i)).collect::<rusqlite::Result<Vec<_>>>()
    }).unwrap();
    let mut bag = BTreeMap::new();
    for row in raw { *bag.entry(row.unwrap()).or_insert(0) += 1; }
    bag.into_iter().collect()
}

fn run<E: Engine>(steps: Vec<(u32, String, i64)>) {
    let program: Program = serde_json::from_str(include_str!("../../ivm-dd/oracle/16_string.program.json")).unwrap();
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch(include_str!("../../ivm-dd/oracle/16_string.sql")).unwrap();
    let db = Connection::open_in_memory().unwrap();
    let mut engine = E::install(&program, &mut Raw::with_connection(&db)).unwrap();
    let names = [(2, "greeting"), (3, "split"), (4, "empty"), (5, "ordering"), (6, "roundtrip")];
    for (at, (rel, value, w)) in steps.into_iter().enumerate() {
        let mut before = BTreeMap::new();
        for (_, name) in names { before.insert(name, oracle_rows(&oracle, name)); }
        let source = if rel == 0 { "name" } else { "word" };
        let statement = if w > 0 { format!("INSERT INTO {source} VALUES (?1)") } else { format!("DELETE FROM {source} WHERE c0=?1") };
        oracle.execute(&statement, [&value]).unwrap();
        let id = engine.intern_text(&value, &mut Raw::with_connection(&db)).unwrap();
        assert_eq!(engine.text(id, &mut Raw::with_connection(&db)).unwrap().as_deref(), Some(value.as_str()));
        let delta = engine.settle(Frontier { changes: vec![SourceChange { rel, row: vec![id], w }] }, &mut Raw::with_connection(&db)).unwrap_or_else(|e| panic!("step {at}: {e}"));
        let Delta { changes, .. } = delta;
        let mut actual_delta = changes.into_iter().map(|(rel, row, weight)| {
            let value = rows(&engine, &db, vec![(row, weight)]).pop().unwrap();
            (rel, value.0, value.1)
        }).collect::<Vec<_>>();
        actual_delta.sort();
        let mut expected_delta = Vec::new();
        for (id, name) in names {
            let after = oracle_rows(&oracle, name);
            let mut net: BTreeMap<Vec<String>, i64> = BTreeMap::new();
            for (row, weight) in &after { *net.entry(row.clone()).or_default() += weight; }
            for (row, weight) in &before[name] { *net.entry(row.clone()).or_default() -= weight; }
            expected_delta.extend(net.into_iter().filter(|(_, weight)| *weight != 0).map(|(row, weight)| (id, row, weight)));
            let raw = engine.snapshot(id, &mut Raw::with_connection(&db)).unwrap();
            assert_eq!(rows(&engine, &db, raw), after, "step {at} {name} snapshot");
        }
        expected_delta.sort();
        assert_eq!(actual_delta, expected_delta, "step {at} delta");
    }
}

fn script_steps() -> Vec<(u32, String, i64)> {
    [(0, "ada", 1), (1, "hello", 1), (1, "", 1), (1, "écho", 1),
     (1, "hello", -1), (1, "hello", 1), (0, "ada", -1), (0, "ada", 1)]
        .into_iter().map(|(rel, text, w)| (rel, text.to_owned(), w)).collect()
}

fn random_steps(seed: u64) -> Vec<(u32, String, i64)> {
    let mut rng = rng::Rng(seed);
    let alphabet = ["", "a", "b", "é", "😀", "hello", "hi ", "a😀b", "écho"];
    let mut live = [std::collections::BTreeSet::new(), std::collections::BTreeSet::new()];
    (0..12).map(|_| {
        let rel = rng.below(2);
        let text = alphabet[rng.below(alphabet.len())].to_owned();
        let w = if live[rel].remove(&text) { -1 } else { live[rel].insert(text.clone()); 1 };
        (rel as u32, text, w)
    }).collect()
}

#[test]
fn string_script_dd() { run::<Dd>(script_steps()); }

#[test]
fn string_script_sqlite() { run::<Sqlite>(script_steps()); }

#[test]
fn random_string_dd() {
    for seed in rng::seeds(200) { run::<Dd>(random_steps(seed)); }
}

#[test]
fn random_string_sqlite() {
    for seed in rng::seeds(200) { run::<Sqlite>(random_steps(seed)); }
}

fn nul_roundtrip<E: Engine>() {
    let program: Program = serde_json::from_str(include_str!("../../ivm-dd/oracle/16_string.program.json")).unwrap();
    let db = Connection::open_in_memory().unwrap();
    let mut engine = E::install(&program, &mut Raw::with_connection(&db)).unwrap();
    let id = engine.intern_text("a\0b", &mut Raw::with_connection(&db)).unwrap();
    engine.settle(Frontier { changes: vec![SourceChange { rel: 1, row: vec![id], w: 1 }] }, &mut Raw::with_connection(&db)).unwrap();
    assert_eq!(rows(&engine, &db, engine.snapshot(3, &mut Raw::with_connection(&db)).unwrap()), vec![(vec!["a".into(), "\0b".into()], 1)]);
    assert_eq!(rows(&engine, &db, engine.snapshot(6, &mut Raw::with_connection(&db)).unwrap()), vec![(vec!["a\0b".into()], 1)]);
}

#[test]
fn nul_string_dd() { nul_roundtrip::<Dd>(); }

#[test]
fn nul_string_sqlite() { nul_roundtrip::<Sqlite>(); }
