//! Oracle harness: one `.sql` file drives both the frontier and the expected delta of every step.

use lab_20260924_0::{Dd, Delta, Frontier, Program, RelId, RelKind, Row, SourceChange, W};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

type Bag = BTreeMap<Row, W>;

fn oracle_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("oracle")
}

fn read(conn: &Connection, table: &str) -> Bag {
    let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
    let width = stmt.column_count();
    let rows = stmt
        .query_map([], |r| (0..width).map(|i| r.get::<_, i64>(i)).collect::<Result<Row, _>>())
        .unwrap();
    let mut bag = Bag::new();
    for row in rows {
        *bag.entry(row.unwrap()).or_default() += 1;
    }
    bag
}

fn diff(before: &Bag, after: &Bag) -> Vec<(Row, W)> {
    let mut rows: BTreeMap<Row, W> = BTreeMap::new();
    for (row, w) in after {
        *rows.entry(row.clone()).or_default() += w;
    }
    for (row, w) in before {
        *rows.entry(row.clone()).or_default() -= w;
    }
    rows.into_iter().filter(|(_, w)| *w != 0).collect()
}

fn steps(sql: &str) -> (String, Vec<(String, String)>) {
    let mut chunks = sql.split("\n-- step: ");
    let setup = chunks.next().unwrap().to_string();
    let steps = chunks
        .map(|chunk| {
            let (name, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
            (name.trim().to_string(), body.to_string())
        })
        .collect();
    (setup, steps)
}

/// Runs every step in file order; a failing step names itself and prints the marble transcript.
pub fn run(name: &str) -> String {
    let sql = std::fs::read_to_string(oracle_dir().join(format!("{name}.sql"))).unwrap();
    let json = std::fs::read_to_string(oracle_dir().join(format!("{name}.program.json"))).unwrap();
    let program: Program = serde_json::from_str(&json).unwrap();
    let (setup, steps) = steps(&sql);
    assert!(!steps.is_empty(), "{name}: no steps");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&setup).unwrap();
    let sources: Vec<(RelId, String)> = program
        .rels
        .iter()
        .filter(|r| r.kind == RelKind::Source)
        .map(|r| (r.id, r.name.clone()))
        .collect();
    let outputs: Vec<(RelId, String)> = program
        .outputs
        .iter()
        .map(|id| (*id, program.rel(*id).unwrap().name.clone()))
        .collect();
    let snap = |rels: &[(RelId, String)]| -> Vec<Bag> { rels.iter().map(|(_, t)| read(&conn, t)).collect() };

    let mut engine = Dd::install(&program).unwrap();
    let mut marbles = String::from("step\ttick\trel\trow\tw\n");
    for (step, body) in &steps {
        let (src0, out0) = (snap(&sources), snap(&outputs));
        conn.execute_batch(body).unwrap_or_else(|e| panic!("{name}/{step}: oracle SQL failed: {e}"));
        let (src1, out1) = (snap(&sources), snap(&outputs));

        let mut frontier = Frontier::default();
        for (i, (rel, _)) in sources.iter().enumerate() {
            for (row, w) in diff(&src0[i], &src1[i]) {
                frontier.changes.push(SourceChange { rel: *rel, row, w });
            }
        }
        let mut expected: Vec<(RelId, Row, W)> = Vec::new();
        for (i, (rel, _)) in outputs.iter().enumerate() {
            expected.extend(diff(&out0[i], &out1[i]).into_iter().map(|(row, w)| (*rel, row, w)));
        }
        expected.sort();

        let Delta { tick, changes } = engine.settle(frontier).unwrap_or_else(|e| panic!("{name}/{step}: {e}"));
        for (rel, row, w) in &changes {
            writeln!(marbles, "{step}\t{tick}\t{rel}\t{row:?}\t{w}").unwrap();
        }
        let mut keys: Vec<(RelId, &Row)> = changes.iter().map(|(rel, row, _)| (*rel, row)).collect();
        keys.dedup();
        assert_eq!(keys.len(), changes.len(), "{name}/{step}: repeated (rel,row) in raw delta\n{marbles}");
        assert!(changes.iter().all(|(_, _, w)| *w != 0), "{name}/{step}: zero weight in raw delta\n{marbles}");
        assert_eq!(changes, expected, "{name}/{step}: delta\n{marbles}");

        for (i, (rel, _)) in outputs.iter().enumerate() {
            let mut got = engine.snapshot(*rel).unwrap();
            got.sort();
            let want: Vec<(Row, W)> = out1[i].clone().into_iter().collect();
            assert_eq!(got, want, "{name}/{step}: snapshot of rel {rel}\n{marbles}");
        }
    }
    marbles
}
