//! One case end to end: SQLite views recompute each output bag per frontier; the engine's delta must equal the bag diff.
//! A failing case is shrunk, written to tests/corpus/<seed>/, and replayed first on every later run.

use super::{gen, rng::Rng, sql};
use lab_20260924_0::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

type Bag = BTreeMap<Row, W>;

#[derive(Clone, Debug)]
pub struct Case {
    pub seed: u64,
    pub program: Program,
    pub frontiers: Vec<Frontier>,
}

impl Case {
    pub fn generate(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let program = gen::program(&mut rng);
        let frontiers = gen::frontiers(&mut rng, &program);
        Case { seed, program, frontiers }
    }

    pub fn generate_k5(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let program = gen::k5_program();
        let frontiers = gen::frontiers(&mut rng, &program);
        Case { seed, program, frontiers }
    }
}

/// A check returns its first disagreement as text.
pub type Check = fn(&Case) -> Result<(), String>;

struct Oracle {
    conn: Connection,
    outputs: Vec<(RelId, String)>,
}

fn sql_err(e: rusqlite::Error) -> String {
    format!("oracle: {e}")
}

impl Oracle {
    fn new(p: &Program) -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(sql_err)?;
        conn.execute_batch(&sql::ddl(p)).map_err(sql_err)?;
        let outputs = p.outputs.iter().map(|id| (*id, p.rel(*id).unwrap().name.clone())).collect();
        Ok(Oracle { conn, outputs })
    }

    /// One transaction; every statement must touch exactly one row, or the harness broke the set contract.
    fn apply(&self, p: &Program, f: &Frontier) -> Result<(), String> {
        self.conn.execute_batch("BEGIN").map_err(sql_err)?;
        for c in &f.changes {
            let rel = p.rel(c.rel).unwrap();
            let params = rusqlite::params_from_iter(c.row.iter());
            let stmt = if c.w > 0 {
                format!("INSERT INTO \"{}\" VALUES ({})", rel.name, vec!["?"; c.row.len()].join(", "))
            } else {
                let wheres: Vec<String> = (0..c.row.len()).map(|i| format!("c{i} = ?")).collect();
                format!("DELETE FROM \"{}\" WHERE {}", rel.name, wheres.join(" AND "))
            };
            let n = self.conn.execute(&stmt, params).map_err(sql_err)?;
            if n != 1 {
                return Err(format!("oracle: {stmt} {:?} touched {n} rows", c.row));
            }
        }
        self.conn.execute_batch("COMMIT").map_err(sql_err)
    }

    fn bags(&self) -> Result<Vec<Bag>, String> {
        self.outputs.iter().map(|(_, name)| self.bag(name)).collect()
    }

    fn bag(&self, name: &str) -> Result<Bag, String> {
        let mut stmt = self.conn.prepare(&format!("SELECT * FROM \"{name}\"")).map_err(sql_err)?;
        let width = stmt.column_count();
        let rows = stmt
            .query_map([], |r| (0..width).map(|i| r.get::<_, i64>(i)).collect::<Result<Row, _>>())
            .map_err(sql_err)?;
        let mut bag = Bag::new();
        for row in rows {
            *bag.entry(row.map_err(sql_err)?).or_default() += 1;
        }
        Ok(bag)
    }
}

/// Raw delta invariants, checked before any sort or merge.
pub fn raw(d: &Delta) -> Result<(), String> {
    let mut keys: Vec<(RelId, &Row)> = d.changes.iter().map(|(rel, row, _)| (*rel, row)).collect();
    keys.sort();
    keys.dedup();
    if keys.len() != d.changes.len() {
        return Err(format!("repeated (rel,row) in raw delta {:?}", d.changes));
    }
    if d.changes.iter().any(|(_, _, w)| *w == 0) {
        return Err(format!("zero weight in raw delta {:?}", d.changes));
    }
    Ok(())
}

pub fn snapshot<E: Engine>(e: &E, rel: RelId) -> Result<Vec<(Row, W)>, String> {
    let mut rows = e.snapshot(rel).map_err(|e| format!("snapshot: {e}"))?;
    rows.sort();
    Ok(rows)
}

/// The differential check against the SQLite recompute oracle.
pub fn oracle<E: Engine>(case: &Case) -> Result<(), String> {
    let p = &case.program;
    let oracle = Oracle::new(p)?;
    let mut engine = E::install(p).map_err(|e| format!("install: {e}"))?;
    let mut before = oracle.bags()?;
    for (i, f) in case.frontiers.iter().enumerate() {
        oracle.apply(p, f)?;
        let after = oracle.bags()?;
        let delta = engine.settle(f.clone()).map_err(|e| format!("frontier {i}: settle: {e}"))?;
        raw(&delta).map_err(|e| format!("frontier {i}: {e}"))?;
        let mut expected = Vec::new();
        for (k, (rel, _)) in oracle.outputs.iter().enumerate() {
            let mut net = after[k].clone();
            for (row, w) in &before[k] {
                *net.entry(row.clone()).or_default() -= w;
            }
            expected.extend(net.into_iter().filter(|(_, w)| *w != 0).map(|(row, w)| (*rel, row, w)));
        }
        expected.sort();
        if delta.changes != expected {
            return Err(format!("frontier {i}: delta\n  expected {expected:?}\n  got      {:?}", delta.changes));
        }
        for (k, (rel, _)) in oracle.outputs.iter().enumerate() {
            let got = snapshot(&engine, *rel)?;
            let want: Vec<(Row, W)> = after[k].clone().into_iter().collect();
            if got != want {
                return Err(format!("frontier {i}: snapshot of rel {rel}\n  expected {want:?}\n  got      {got:?}"));
            }
        }
        before = after;
    }
    Ok(())
}

fn guarded(check: Check, case: &Case) -> Result<(), String> {
    catch_unwind(AssertUnwindSafe(|| check(case))).unwrap_or_else(|e| {
        let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
        Err(format!("panic: {}", msg.unwrap_or_default()))
    })
}

/// Greedy shrink: shortest failing prefix, then drop frontiers, changes and extra outputs while it still fails.
fn shrink(case: &Case, check: Check) -> Case {
    let fails = |c: &Case| gen::valid(&c.frontiers) && guarded(check, c).is_err();
    let mut best = case.clone();
    for len in 0..=case.frontiers.len() {
        let c = Case { frontiers: case.frontiers[..len].to_vec(), ..case.clone() };
        if fails(&c) {
            best = c;
            break;
        }
    }
    loop {
        let mut progress = false;
        let mut i = 0;
        while i < best.frontiers.len() {
            let mut c = best.clone();
            c.frontiers.remove(i);
            if fails(&c) {
                (best, progress) = (c, true);
            } else {
                i += 1;
            }
        }
        for i in 0..best.frontiers.len() {
            let mut j = 0;
            while j < best.frontiers[i].changes.len() {
                let mut c = best.clone();
                c.frontiers[i].changes.remove(j);
                if fails(&c) {
                    (best, progress) = (c, true);
                } else {
                    j += 1;
                }
            }
        }
        for o in 0..best.program.outputs.len() {
            if best.program.outputs.len() > 1 {
                let mut c = best.clone();
                c.program.outputs.remove(o);
                if fails(&c) {
                    (best, progress) = (c, true);
                    break;
                }
            }
        }
        if !progress {
            return best;
        }
    }
}

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn frontier_sql(p: &Program, frontiers: &[Frontier]) -> String {
    let mut out = String::new();
    for (i, f) in frontiers.iter().enumerate() {
        writeln!(out, "-- step: f{i}\nBEGIN;").unwrap();
        for c in &f.changes {
            let name = &p.rel(c.rel).unwrap().name;
            let cells: Vec<String> = c.row.iter().map(|v| v.to_string()).collect();
            if c.w > 0 {
                writeln!(out, "INSERT INTO \"{name}\" VALUES ({});", cells.join(", ")).unwrap();
            } else {
                let wheres: Vec<String> = cells.iter().enumerate().map(|(i, v)| format!("c{i} = {v}")).collect();
                writeln!(out, "DELETE FROM \"{name}\" WHERE {};", wheres.join(" AND ")).unwrap();
            }
        }
        writeln!(out, "COMMIT;").unwrap();
    }
    out
}

fn write_corpus(test: &str, case: &Case, msg: &str) {
    let dir = corpus_dir().join(case.seed.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("seed"), format!("{}\n", case.seed)).unwrap();
    std::fs::write(dir.join("program.json"), serde_json::to_string_pretty(&case.program).unwrap()).unwrap();
    std::fs::write(dir.join("frontiers.json"), serde_json::to_string_pretty(&case.frontiers).unwrap()).unwrap();
    let oracle_sql = format!("{}{}", sql::ddl(&case.program), frontier_sql(&case.program, &case.frontiers));
    std::fs::write(dir.join("oracle.sql"), oracle_sql).unwrap();
    std::fs::write(dir.join("failure.txt"), format!("{test}\n{msg}\n")).unwrap();
}

fn corpus() -> Vec<Case> {
    let Ok(dirs) = std::fs::read_dir(corpus_dir()) else { return vec![] };
    let mut cases: Vec<Case> = dirs
        .filter_map(|d| {
            let dir = d.ok()?.path();
            let seed = dir.file_name()?.to_str()?.parse().ok()?;
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
            let program = serde_json::from_str(&read("program.json")).unwrap();
            let frontiers = serde_json::from_str(&read("frontiers.json")).unwrap();
            Some(Case { seed, program, frontiers })
        })
        .collect();
    cases.sort_by_key(|c| c.seed);
    cases
}

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().map(|v| v.parse().unwrap())
}

/// `RANDOM_SEED` runs one seed; otherwise `SEEDS` (or `RANDOM_CASES`) seeds from `RANDOM_BASE`.
pub fn seeds(cases: u64) -> Vec<u64> {
    if let Some(seed) = env("RANDOM_SEED") {
        return vec![seed];
    }
    let base = env("RANDOM_BASE").unwrap_or(0);
    (base..base + env("SEEDS").or_else(|| env("RANDOM_CASES")).unwrap_or(cases)).collect()
}

/// Corpus first, then generated seeds; the first failure is shrunk, saved, and panics with a rerun command.
pub fn run(test: &str, cases: u64, generate: fn(u64) -> Case, check: Check) {
    let generated = seeds(cases).into_iter().map(generate);
    for case in corpus().into_iter().chain(generated) {
        let Err(msg) = guarded(check, &case) else { continue };
        let small = shrink(&case, check);
        let small_msg = guarded(check, &small).err().unwrap_or(msg);
        write_corpus(test, &small, &small_msg);
        panic!(
            "{test} seed {seed}: {small_msg}\nprogram: {}\nfrontiers: {}\ncorpus: {}\nrerun: cd {} && RANDOM_SEED={seed} CARGO_TARGET_DIR=../20260923.3.dd-inside-sqlite/target cargo test --offline -j 2 --test 2_random {test} -- --nocapture",
            serde_json::to_string(&small.program).unwrap(),
            serde_json::to_string(&small.frontiers).unwrap(),
            corpus_dir().join(small.seed.to_string()).display(),
            env!("CARGO_MANIFEST_DIR"),
            seed = small.seed,
        );
    }
}
