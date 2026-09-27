//! Oracle harness, generic over `Engine`: one step file drives both the frontier and the expected delta.
//! A step body is SQL (frontier = source diff) or raw `+ table cells` / `- table cells` lines (frontier = those lines, in order).
#![allow(dead_code)]

use ivm_dd::{Delta, Engine, Frontier, NodeId, Program, RelId, RelKind, Row, SourceChange, W};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

type Bag = BTreeMap<Row, W>;

pub fn program(name: &str) -> Program {
    let json = std::fs::read_to_string(oracle_dir().join(format!("{name}.program.json"))).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn oracle_dir() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let own = crate_dir.join("oracle");
    if own.exists() { own } else { crate_dir.join("../ivm-dd/oracle") }
}

pub fn read(conn: &Connection, table: &str) -> Bag {
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

pub fn columns(conn: &Connection, table: &str) -> Vec<String> {
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

/// A script read from `oracle/<name>.sql` with its program and header comments.
pub struct Script {
    pub name: String,
    pub program: Program,
    pub setup: String,
    /// `-- title:`, `-- question:`; "" when absent.
    pub title: String,
    pub question: String,
    /// `-- name: <value> <label>`, file order.
    pub names: Vec<(String, String)>,
    /// `-- node: <ir node id> <caption>`, file order.
    pub captions: Vec<(NodeId, String)>,
    steps: Vec<Step>,
}

/// One step as the SQLite oracle saw it; engine independent.
pub struct OracleStep {
    pub caption: String,
    pub frontier: Frontier,
    pub expect_error: Option<String>,
    /// Output view diff sorted by `(rel, row)`, or the oracle SQL error.
    pub oracle: Result<Vec<(RelId, Row, W)>, String>,
    /// Output views after the step, in `program.outputs` order.
    after: Vec<Bag>,
}

/// `name` is relative to `oracle/`, without `.sql`, e.g. `pokemon/0_can_surf`.
pub fn script(name: &str) -> Script {
    let text = std::fs::read_to_string(oracle_dir().join(format!("{name}.sql"))).unwrap();
    let (program_name, setup, steps) = parse(&text);
    let program_name = program_name.unwrap_or_else(|| name.to_string());
    let json = std::fs::read_to_string(oracle_dir().join(format!("{program_name}.program.json"))).unwrap();
    let program: Program = serde_json::from_str(&json).unwrap();
    let header = |key: &str| setup.lines().find_map(|l| l.strip_prefix(key)).map(|v| v.trim().to_string()).unwrap_or_default();
    let pairs = |key: &str| -> Vec<(String, String)> {
        setup
            .lines()
            .filter_map(|l| l.strip_prefix(key))
            .map(|v| {
                let (k, label) = v.trim().split_once(' ').unwrap_or((v.trim(), ""));
                (k.to_string(), label.trim().to_string())
            })
            .collect()
    };
    Script {
        name: name.to_string(),
        title: header("-- title: "),
        question: header("-- question: "),
        names: pairs("-- name: "),
        captions: pairs("-- node: ").into_iter().map(|(k, c)| (k.parse().unwrap(), c)).collect(),
        program,
        setup,
        steps,
    }
}

/// Runs setup and every step against SQLite; returns the connection after the last step.
pub fn oracle(script: &Script) -> (Connection, Vec<OracleStep>) {
    let program = &script.program;
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&script.setup).unwrap();
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
    let mut out = Vec::new();
    for step in &script.steps {
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
        let oracle = oracle.map_err(|e| e.to_string()).map(|()| {
            let mut expected: Vec<(RelId, Row, W)> = Vec::new();
            for (i, (rel, _)) in outputs.iter().enumerate() {
                expected.extend(diff(&out0[i], &out1[i]).into_iter().map(|(row, w)| (*rel, row, w)));
            }
            expected.sort();
            expected
        });
        out.push(OracleStep { caption: step.name.clone(), frontier, expect_error: step.expect_error.clone(), oracle, after: out1 });
    }
    (conn, out)
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

/// A script header's `-- expect-error:` names an install error for an engine that cannot run it.
pub fn expect_install_error<E: Engine>(name: &str) {
    let script = script(name);
    let kind = script.setup.lines().find_map(|line| line.strip_prefix("-- expect-error: ")).expect("missing install error expectation");
    let db = Connection::open_in_memory().unwrap();
    match E::install(&script.program, &mut ivm_dd::Raw::with_connection(&db)) {
        Ok(_) => panic!("{name}: expected install error {kind}"),
        Err(e) => assert!(format!("{:?}", e.kind).starts_with(kind), "{name}: expected {kind}, got {e}"),
    }
}

fn run_steps<E: Engine>(name: &str) -> String {
    let script = script(name);
    let program = &script.program;
    assert!(!script.steps.is_empty(), "{name}: no steps");
    let (_conn, steps) = oracle(&script);

    let engine_db = Connection::open_in_memory().unwrap();
    let mut host = ivm_dd::Raw::with_connection(&engine_db);
    let mut engine = E::install(program, &mut host).unwrap();
    let mut marbles = String::from("step\ttick\trel\trow\tw\n");
    for step in steps {
        let at = format!("{name}/{}", step.caption);
        let settled = engine.settle(step.frontier, &mut host);
        match (&step.expect_error, step.oracle, settled) {
            (Some(kind), Err(_), Err(e)) => {
                assert!(format!("{:?}", e.kind).starts_with(kind.as_str()), "{at}: expected {kind}, got {e}");
                writeln!(marbles, "{}\t-\t-\terror {kind}\t0", step.caption).unwrap();
            }
            (Some(kind), oracle, settled) => panic!("{at}: expected {kind} from both; oracle {oracle:?}, engine {settled:?}"),
            (None, Err(e), _) => panic!("{at}: oracle SQL failed: {e}"),
            (None, Ok(_), Err(e)) => panic!("{at}: {e}\n{marbles}"),
            (None, Ok(expected), Ok(Delta { tick, changes })) => {
                for (rel, row, w) in &changes {
                    writeln!(marbles, "{}\t{tick}\t{rel}\t{row:?}\t{w}", step.caption).unwrap();
                }
                let mut keys: Vec<(RelId, &Row)> = changes.iter().map(|(rel, row, _)| (*rel, row)).collect();
                keys.dedup();
                assert_eq!(keys.len(), changes.len(), "{at}: repeated (rel,row) in raw delta\n{marbles}");
                assert!(changes.iter().all(|(_, _, w)| *w != 0), "{at}: zero weight in raw delta\n{marbles}");
                assert_eq!(changes, expected, "{at}: delta\n{marbles}");
            }
        }
        for (i, rel) in program.outputs.iter().enumerate() {
            let mut got = engine.snapshot(*rel, &mut host).unwrap();
            got.sort();
            let want: Vec<(Row, W)> = step.after[i].clone().into_iter().collect();
            assert_eq!(got, want, "{at}: snapshot of rel {rel}\n{marbles}");
        }
    }
    marbles
}
