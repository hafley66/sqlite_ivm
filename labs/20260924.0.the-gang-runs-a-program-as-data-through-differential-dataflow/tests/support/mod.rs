//! Oracle harness, generic over `Engine`: one step file drives both the frontier and the expected delta.
//! A step body is SQL (frontier = source diff) or raw `+ table cells` / `- table cells` lines (frontier = those lines, in order).

use lab_20260924_0::{Delta, Engine, Frontier, Program, RelId, RelKind, Row, SourceChange, W};
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

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid")).unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
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

struct Step {
    name: String,
    sql: String,
    raw: Vec<(String, Row, W)>,
    expect_error: Option<String>,
}

fn parse(text: &str) -> (Option<String>, String, Vec<Step>) {
    let mut chunks = text.split("\n-- step: ");
    let setup = chunks.next().unwrap().to_string();
    let program = setup
        .lines()
        .find_map(|l| l.strip_prefix("-- program: "))
        .map(|p| p.trim().to_string());
    let steps = chunks
        .map(|chunk| {
            let (name, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
            let mut step = Step { name: name.trim().to_string(), sql: String::new(), raw: Vec::new(), expect_error: None };
            for line in body.lines() {
                let mut words = line.split_whitespace();
                match words.next() {
                    Some(sign @ ("+" | "-")) => {
                        let table = words.next().unwrap().to_string();
                        let row: Row = words.map(|w| w.parse().unwrap()).collect();
                        step.raw.push((table, row, if sign == "+" { 1 } else { -1 }));
                    }
                    _ => match line.strip_prefix("-- expect-error: ") {
                        Some(kind) => step.expect_error = Some(kind.trim().to_string()),
                        None => writeln!(step.sql, "{line}").unwrap(),
                    },
                }
            }
            step
        })
        .collect();
    (program, setup, steps)
}

/// Applies raw lines to SQLite in one transaction; a failing statement rolls the whole step back.
fn apply_raw(conn: &Connection, raw: &[(String, Row, W)]) -> Result<(), rusqlite::Error> {
    conn.execute_batch("BEGIN")?;
    let result = raw.iter().try_for_each(|(table, row, w)| {
        let params = rusqlite::params_from_iter(row.iter());
        if *w > 0 {
            let holes = vec!["?"; row.len()].join(",");
            conn.execute(&format!("INSERT INTO \"{table}\" VALUES ({holes})"), params).map(drop)
        } else {
            let wheres: Vec<String> = columns(conn, table).iter().map(|c| format!("\"{c}\" = ?")).collect();
            conn.execute(&format!("DELETE FROM \"{table}\" WHERE {}", wheres.join(" AND ")), params).map(drop)
        }
    });
    match result {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            conn.execute_batch("ROLLBACK")?;
            Err(e)
        }
    }
}

/// Runs every step in file order and returns the marble transcript `step tick rel row w`.
/// A script that does not finish in 10s fails; a diverging fixpoint must not hang the suite.
pub fn run<E: Engine + 'static>(name: &str) -> String {
    let (done, finished) = std::sync::mpsc::channel();
    let owned = name.to_string();
    let worker = std::thread::spawn(move || {
        let marbles = run_steps::<E>(&owned);
        let _ = done.send(());
        marbles
    });
    match finished.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(()) => worker.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(worker.join().unwrap_err()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("{name}: no result within 10s"),
    }
}

fn run_steps<E: Engine>(name: &str) -> String {
    let text = std::fs::read_to_string(oracle_dir().join(format!("{name}.sql"))).unwrap();
    let (program_name, setup, steps) = parse(&text);
    let program_name = program_name.unwrap_or_else(|| name.to_string());
    let json = std::fs::read_to_string(oracle_dir().join(format!("{program_name}.program.json"))).unwrap();
    let program: Program = serde_json::from_str(&json).unwrap();
    assert!(!steps.is_empty(), "{name}: no steps");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&setup).unwrap();
    let rel_of = |table: &str| program.rels.iter().find(|r| r.name == table).unwrap().id;
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

    let mut engine = E::install(&program).unwrap();
    let mut marbles = String::from("step\ttick\trel\trow\tw\n");
    for step in &steps {
        let at = format!("{name}/{}", step.name);
        let (src0, out0) = (snap(&sources), snap(&outputs));
        let oracle = if step.raw.is_empty() { conn.execute_batch(&step.sql) } else { apply_raw(&conn, &step.raw) };
        let (src1, out1) = (snap(&sources), snap(&outputs));

        let mut frontier = Frontier::default();
        if step.raw.is_empty() {
            for (i, (rel, _)) in sources.iter().enumerate() {
                for (row, w) in diff(&src0[i], &src1[i]) {
                    frontier.changes.push(SourceChange { rel: *rel, row, w });
                }
            }
        } else {
            for (table, row, w) in &step.raw {
                frontier.changes.push(SourceChange { rel: rel_of(table), row: row.clone(), w: *w });
            }
        }

        let settled = engine.settle(frontier);
        match (&step.expect_error, oracle, settled) {
            (Some(kind), Err(_), Err(e)) => {
                assert!(format!("{:?}", e.kind).starts_with(kind.as_str()), "{at}: expected {kind}, got {e}");
                writeln!(marbles, "{}\t-\t-\terror {kind}\t0", step.name).unwrap();
            }
            (Some(kind), oracle, settled) => panic!("{at}: expected {kind} from both; oracle {oracle:?}, engine {settled:?}"),
            (None, Err(e), _) => panic!("{at}: oracle SQL failed: {e}"),
            (None, Ok(()), Err(e)) => panic!("{at}: {e}\n{marbles}"),
            (None, Ok(()), Ok(Delta { tick, changes })) => {
                let mut expected: Vec<(RelId, Row, W)> = Vec::new();
                for (i, (rel, _)) in outputs.iter().enumerate() {
                    expected.extend(diff(&out0[i], &out1[i]).into_iter().map(|(row, w)| (*rel, row, w)));
                }
                expected.sort();
                for (rel, row, w) in &changes {
                    writeln!(marbles, "{}\t{tick}\t{rel}\t{row:?}\t{w}", step.name).unwrap();
                }
                let mut keys: Vec<(RelId, &Row)> = changes.iter().map(|(rel, row, _)| (*rel, row)).collect();
                keys.dedup();
                assert_eq!(keys.len(), changes.len(), "{at}: repeated (rel,row) in raw delta\n{marbles}");
                assert!(changes.iter().all(|(_, _, w)| *w != 0), "{at}: zero weight in raw delta\n{marbles}");
                assert_eq!(changes, expected, "{at}: delta\n{marbles}");
            }
        }
        for (i, (rel, _)) in outputs.iter().enumerate() {
            let mut got = engine.snapshot(*rel).unwrap();
            got.sort();
            let want: Vec<(Row, W)> = out1[i].clone().into_iter().collect();
            assert_eq!(got, want, "{at}: snapshot of rel {rel}\n{marbles}");
        }
    }
    marbles
}
