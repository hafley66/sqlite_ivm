//! One case end to end: SQLite views recompute each output bag per frontier; the engine's delta must equal the bag diff.
//! A failing case is shrunk, written to tests/corpus/<seed>/, and replayed first on every later run.

use super::{gen, rng::Rng, sql};
use ivm_dd::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

type Bag = BTreeMap<Row, W>;

pub fn memory_connection() -> rusqlite::Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA temp_store = MEMORY;")?;
    Ok(db)
}

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

    pub fn generate_typed(seed: u64) -> Self {
        let mut rng = Rng(seed);
        Case { seed, program: gen::typed_program(&mut rng), frontiers: gen::typed_frontiers(&mut rng) }
    }

    pub fn generate_mint(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let args = if rng.chance(50) { vec![0, 1] } else { vec![1, 0] };
        let program = Program {
            texts: vec![],
            rels: vec![
                Relation { id: 0, name: "mint_source".into(), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Source },
                Relation { id: 1, name: "mint_pair".into(), cols: vec![Ty::Id, Ty::Int, Ty::Int], kind: RelKind::Constructor },
                Relation { id: 2, name: "mint_wrap".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Constructor },
                Relation { id: 3, name: "mint_pairs".into(), cols: vec![Ty::Int, Ty::Int, Ty::Id], kind: RelKind::Derived },
                Relation { id: 4, name: "mint_wrapped".into(), cols: vec![Ty::Int, Ty::Int, Ty::Id, Ty::Id], kind: RelKind::Derived },
                Relation { id: 5, name: "mint_pairs_by_id".into(), cols: vec![Ty::Id, Ty::Int, Ty::Int], kind: RelKind::Derived },
                Relation { id: 6, name: "mint_round_trip".into(), cols: vec![Ty::Int, Ty::Int, Ty::Id, Ty::Id, Ty::Int, Ty::Int], kind: RelKind::Derived },
                Relation { id: 7, name: "mint_ordered_pairs".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Derived },
                Relation { id: 8, name: "mint_extrema".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Derived },
                Relation { id: 9, name: "mint_top".into(), cols: vec![Ty::Int, Ty::Int, Ty::Id], kind: RelKind::Derived },
            ],
            nodes: vec![
                Op::Get(0),
                Op::Mint { input: 0, functor: 1, args },
                Op::Mint { input: 1, functor: 2, args: vec![2] },
                Op::Get(1),
                Op::Join { inputs: vec![1, 3], equivalences: vec![vec![(0, 2), (1, 0)]] },
                Op::Join { inputs: vec![1, 1], equivalences: vec![] },
                Op::Mfp { input: 5, filter: vec![Expr::Call(Func::TermLt, vec![Expr::Col(2), Expr::Col(5)])], map: vec![], project: vec![2, 5] },
                Op::Reduce { input: 1, key: vec![], aggs: vec![Agg::Min(2), Agg::Max(2)] },
                Op::TopK { input: 1, key: vec![], order: vec![Order { col: 2, desc: false }], limit: 2 },
            ],
            strata: vec![Stratum::Let { id: 3, body: 1 }, Stratum::Let { id: 4, body: 2 }, Stratum::Let { id: 5, body: 3 }, Stratum::Let { id: 6, body: 4 }, Stratum::Let { id: 7, body: 6 }, Stratum::Let { id: 8, body: 7 }, Stratum::Let { id: 9, body: 8 }],
            outputs: vec![3, 4, 5, 6, 7, 8, 9],
        };
        let mut live = std::collections::BTreeSet::new();
        let mut frontiers = Vec::new();
        for _ in 0..16 {
            let row = vec![rng.range(0, 3) as i64, rng.range(0, 3) as i64];
            let w = if live.remove(&row) { -1 } else { live.insert(row.clone()); 1 };
            frontiers.push(Frontier { changes: vec![SourceChange { rel: 0, row, w }] });
        }
        Case { seed, program, frontiers }
    }

    pub fn generate_recursive_shapes(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let program = gen::recursive_shapes_program(&mut rng);
        let frontiers = gen::frontiers(&mut rng, &program);
        Case { seed, program, frontiers }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Value {
    Int(Cell),
    Atom(Cell),
    Term(String, Vec<Value>),
}

struct Terms {
    by_id: BTreeMap<Cell, (String, Vec<Ty>, Row)>,
}

impl Terms {
    fn read<E: Engine>(p: &Program, engine: &E, db: &Connection) -> Result<Self, String> {
        let mut by_id = BTreeMap::new();
        for rel in p.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
            let rows = engine.intern_snapshot(rel.id, &mut Raw::with_connection(db)).map_err(|e| e.to_string())?;
            for (row, w) in rows {
                if w != 1 { return Err(format!("constructor {} has weight {w}", rel.name)); }
                by_id.insert(row[0], (rel.name.clone(), rel.cols[1..].to_vec(), row[1..].to_vec()));
            }
        }
        Ok(Self { by_id })
    }

    fn value(&self, cell: Cell, ty: Ty, visiting: &mut Vec<Cell>) -> Result<Value, String> {
        if ty == Ty::Int { return Ok(Value::Int(cell)); }
        let Some((name, types, args)) = self.by_id.get(&cell) else { return Ok(Value::Atom(cell)); };
        if visiting.contains(&cell) { return Err(format!("constructor cycle at id {cell}")); }
        visiting.push(cell);
        let values = args.iter().zip(types).map(|(arg, ty)| self.value(*arg, *ty, visiting)).collect::<Result<Vec<_>, _>>()?;
        visiting.pop();
        Ok(Value::Term(name.clone(), values))
    }

    fn row(&self, row: &Row, types: &[Ty]) -> Result<Vec<Value>, String> {
        row.iter().zip(types).map(|(cell, ty)| self.value(*cell, *ty, &mut Vec::new())).collect()
    }

    fn delta(&self, p: &Program, delta: &Delta) -> Result<Vec<(RelId, Vec<Value>, W)>, String> {
        let mut rows = delta.changes.iter().map(|(rel, row, w)| {
            Ok((*rel, self.row(row, &p.rel(*rel).unwrap().cols)?, *w))
        }).collect::<Result<Vec<_>, String>>()?;
        rows.sort();
        Ok(rows)
    }

    fn snapshot<E: Engine>(&self, p: &Program, engine: &E, rel: RelId, db: &Connection) -> Result<Vec<(Vec<Value>, W)>, String> {
        let mut rows = snapshot(engine, rel, db)?.into_iter().map(|(row, w)| {
            Ok((self.row(&row, &p.rel(rel).unwrap().cols)?, w))
        }).collect::<Result<Vec<_>, String>>()?;
        rows.sort();
        Ok(rows)
    }
}

fn agree_frontier<A: Engine, B: Engine>(p: &Program, at: usize, left: &A, left_db: &Connection, a: &Delta, right: &B, right_db: &Connection, b: &Delta) -> Result<(), String> {
    let left_terms = Terms::read(p, left, left_db)?;
    let right_terms = Terms::read(p, right, right_db)?;
    let (a, b) = (left_terms.delta(p, a)?, right_terms.delta(p, b)?);
    if a != b { return Err(format!("frontier {at}: delta {a:?} != {b:?}")); }
    for rel in &p.outputs {
        let a = left_terms.snapshot(p, left, *rel, left_db)?;
        let b = right_terms.snapshot(p, right, *rel, right_db)?;
        if a != b { return Err(format!("frontier {at}: relation {rel} {a:?} != {b:?}")); }
    }
    Ok(())
}

pub fn agreement<A: Engine, B: Engine>(case: &Case) -> Result<(), String> {
    let left_db = memory_connection().map_err(sql_err)?;
    let right_db = memory_connection().map_err(sql_err)?;
    let mut left = A::install(&case.program, &mut Raw::with_connection(&left_db)).map_err(|e| format!("left install: {e}"))?;
    let mut right = B::install(&case.program, &mut Raw::with_connection(&right_db)).map_err(|e| format!("right install: {e}"))?;
    for (at, frontier) in case.frontiers.iter().enumerate() {
        let a = left.settle(frontier.clone(), &mut Raw::with_connection(&left_db)).map_err(|e| format!("left frontier {at}: {e}"))?;
        let b = right.settle(frontier.clone(), &mut Raw::with_connection(&right_db)).map_err(|e| format!("right frontier {at}: {e}"))?;
        agree_frontier(&case.program, at, &left, &left_db, &a, &right, &right_db, &b)?;
    }
    Ok(())
}

pub fn term_lt_structure<E: Engine>(case: &Case) -> Result<(), String> {
    let db = memory_connection().map_err(sql_err)?;
    let mut engine = E::install(&case.program, &mut Raw::with_connection(&db)).map_err(|e| format!("install: {e}"))?;
    for (at, frontier) in case.frontiers.iter().enumerate() {
        engine.settle(frontier.clone(), &mut Raw::with_connection(&db)).map_err(|e| format!("frontier {at}: {e}"))?;
        let terms = snapshot(&engine, 3, &db)?;
        let ordered = snapshot(&engine, 7, &db)?;
        let mut expected = Vec::new();
        for (a, _) in &terms {
            for (b, _) in &terms {
                let left_args = if let Op::Mint { args, .. } = &case.program.nodes[1] { args.iter().map(|i| a[*i as usize]).collect::<Vec<_>>() } else { unreachable!() };
                let right_args = if let Op::Mint { args, .. } = &case.program.nodes[1] { args.iter().map(|i| b[*i as usize]).collect::<Vec<_>>() } else { unreachable!() };
                if left_args < right_args {
                    expected.push((vec![a[2], b[2]], 1));
                }
            }
        }
        expected.sort();
        if ordered != expected { return Err(format!("frontier {at}: TermLt {ordered:?} != structural {expected:?}")); }
    }
    Ok(())
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
        let conn = memory_connection().map_err(sql_err)?;
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

pub fn snapshot<E: Engine>(e: &E, rel: RelId, db: &Connection) -> Result<Vec<(Row, W)>, String> {
    let mut rows = e.snapshot(rel, &mut Raw::with_connection(db)).map_err(|e| format!("snapshot: {e}"))?;
    rows.sort();
    Ok(rows)
}

/// The differential check against the SQLite recompute oracle.
pub fn oracle<E: Engine>(case: &Case) -> Result<(), String> {
    let p = &case.program;
    let oracle = Oracle::new(p)?;
    let engine_db = memory_connection().map_err(sql_err)?;
    let mut engine = E::install(p, &mut Raw::with_connection(&engine_db)).map_err(|e| format!("install: {e}"))?;
    let mut before = oracle.bags()?;
    for (i, f) in case.frontiers.iter().enumerate() {
        oracle.apply(p, f)?;
        let after = oracle.bags()?;
        let delta = engine.settle(f.clone(), &mut Raw::with_connection(&engine_db)).map_err(|e| format!("frontier {i}: settle: {e}"))?;
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
            let got = snapshot(&engine, *rel, &engine_db)?;
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

/// `RANDOM_SEED` runs one seed; otherwise `SEEDS` (or `RANDOM_CASES`) seeds from `RANDOM_BASE`.
pub fn seeds(cases: u64) -> Vec<u64> {
    super::rng::seeds(cases)
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
            "{test} seed {seed}: {small_msg}\nprogram: {}\nfrontiers: {}\ncorpus: {}\nrerun: cd {} && RANDOM_SEED={seed} cargo test -j 4 --test 2_random {test} -- --nocapture",
            serde_json::to_string(&small.program).unwrap(),
            serde_json::to_string(&small.frontiers).unwrap(),
            corpus_dir().join(small.seed.to_string()).display(),
            env!("CARGO_MANIFEST_DIR"),
            seed = small.seed,
        );
    }
}
