//! Real external CLI against DD, using the shared scripts and captured frontiers.
#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_dd::Dd;
use ivm_duckdb::{
    Cell, DuckDb, Engine, EngineError, Frontier, Program, RelId, RelKind, Row, SourceChange, Ty, W,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
type Terms = BTreeMap<Cell, (RelId, Row)>;

fn terms<E: Engine>(p: &Program, e: &E) -> TestResult<Terms> {
    let mut terms = Terms::new();
    for r in p.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
        for (row, w) in e.intern_snapshot(r.id)? {
            if w != 1 || row.is_empty() {
                return Err("invalid constructor row".into());
            }
            terms.insert(row[0], (r.id, row[1..].to_vec()));
        }
    }
    Ok(terms)
}
fn value<E: Engine>(
    p: &Program,
    e: &E,
    terms: &Terms,
    c: Cell,
    t: Ty,
    depth: usize,
) -> TestResult<String> {
    if depth > p.rels.len() + terms.len() {
        return Err("cyclic term".into());
    }
    Ok(match t {
        Ty::Int | Ty::Real => format!("{t:?}:{c}"),
        Ty::Text => format!("text:{:?}", e.text(c)?),
        Ty::Any => format!("any:{:?}", e.any_value(c)?),
        Ty::Id => {
            if let Some(text) = e.text(c)? {
                format!("text:{text:?}")
            } else if let Some((f, args)) = terms.get(&c) {
                let rel = p.rel(*f).ok_or("unknown constructor")?;
                let args = args
                    .iter()
                    .zip(&rel.cols[1..])
                    .map(|(c, t)| value(p, e, terms, *c, *t, depth + 1))
                    .collect::<TestResult<Vec<_>>>()?;
                format!("{}:{args:?}", rel.name)
            } else {
                format!("atom:{c}")
            }
        }
    })
}
fn translate(
    p: &Program,
    dd: &Dd,
    duck: &mut DuckDb,
    terms: &Terms,
    c: Cell,
    t: Ty,
    depth: usize,
) -> TestResult<Cell> {
    if depth > terms.len() + p.rels.len() {
        return Err("cyclic term".into());
    }
    Ok(match t {
        Ty::Int | Ty::Real => c,
        Ty::Text => duck.intern_text(&dd.text(c)?.ok_or("unknown text")?)?,
        Ty::Any => duck.intern_any(&dd.any_value(c)?)?,
        Ty::Id => {
            if let Some(text) = dd.text(c)? {
                duck.intern_text(&text)?
            } else if let Some((f, args)) = terms.get(&c) {
                let rel = p.rel(*f).ok_or("unknown constructor")?;
                let args = args
                    .iter()
                    .zip(&rel.cols[1..])
                    .map(|(c, t)| translate(p, dd, duck, terms, *c, *t, depth + 1))
                    .collect::<TestResult<Row>>()?;
                *duck
                    .intern_terms(&[(*f, args)])?
                    .first()
                    .ok_or("missing intern result")?
            } else {
                c
            }
        }
    })
}
fn canonical<E: Engine>(
    p: &Program,
    e: &E,
    rows: Vec<(RelId, Row, W)>,
) -> TestResult<BTreeMap<(RelId, String), W>> {
    let ts = terms(p, e)?;
    let mut out = BTreeMap::new();
    for (rel, row, w) in rows {
        let types = &p.rel(rel).ok_or("unknown output")?.cols;
        if row.len() != types.len() {
            return Err("wrong output arity".into());
        }
        let cells = row
            .iter()
            .zip(types)
            .map(|(c, t)| value(p, e, &ts, *c, *t, 0))
            .collect::<TestResult<Vec<_>>>()?;
        *out.entry((rel, format!("{cells:?}"))).or_default() += w;
    }
    out.retain(|_, w| *w != 0);
    Ok(out)
}
fn differences(
    at: &str,
    left: BTreeMap<(RelId, String), W>,
    right: BTreeMap<(RelId, String), W>,
    errors: &mut Vec<String>,
) {
    for key in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
        let (a, b) = (
            left.get(key).copied().unwrap_or(0),
            right.get(key).copied().unwrap_or(0),
        );
        if a != b {
            let difference = format!("{at}: rel {} row {} dd={a} duckdb={b}", key.0, key.1);
            println!("DIFF {difference}");
            errors.push(difference)
        }
    }
}
fn compare(name: &str, p: &Program, texts: &[String], frontiers: &[Frontier]) -> TestResult<()> {
    println!("INSTALL {name}");
    let mut dd = Dd::install(p)?;
    let mut duck = DuckDb::install(p)?;
    for text in texts {
        dd.intern_text(text)?;
        duck.intern_text(text)?;
    }
    let mut errors = Vec::new();
    for (i, f) in frontiers.iter().enumerate() {
        let ts = terms(p, &dd)?;
        let changes = f
            .changes
            .iter()
            .map(|change| {
                let rel = p.rel(change.rel).ok_or("unknown source")?;
                let row = change
                    .row
                    .iter()
                    .zip(&rel.cols)
                    .map(|(c, t)| translate(p, &dd, &mut duck, &ts, *c, *t, 0))
                    .collect::<TestResult<Row>>()?;
                Ok(SourceChange {
                    rel: change.rel,
                    row,
                    w: change.w,
                })
            })
            .collect::<TestResult<Vec<_>>>()?;
        let (a, b) = (dd.settle(f.clone()), duck.settle(Frontier { changes }));
        match (a, b) {
            (Ok(a), Ok(b)) => {
                for (label, delta) in [("dd", &a), ("duckdb", &b)] {
                    let mut seen = BTreeSet::new();
                    for (rel, row, w) in &delta.changes {
                        if *w == 0 || !seen.insert((*rel, row)) {
                            let message = format!("{name}/{i}: {label} unconsolidated delta {delta:?}");
                            println!("ERROR_DIFF {message}");
                            errors.push(message);
                        }
                    }
                }
                if a.tick != b.tick {
                    let message = format!("{name}/{i}: tick {} != {}", a.tick, b.tick);
                    println!("ERROR_DIFF {message}");
                    errors.push(message);
                }
                differences(
                    &format!("{name}/{i} delta"),
                    canonical(p, &dd, a.changes)?,
                    canonical(p, &duck, b.changes)?,
                    &mut errors,
                );
            }
            (Err(a), Err(b)) if a.kind == b.kind => {
                println!("REJECT {name}/{i} {:?}", b.kind);
            }
            (a, b) => {
                let message = format!("{name}/{i}: dd={a:?} duckdb={b:?}");
                println!("ERROR_DIFF {message}");
                errors.push(message);
            },
        }
        println!("WORK {name}/{i} {:?}", duck.work());
        let snapshots = |engine: &dyn Snapshot| -> TestResult<Vec<(RelId, Row, W)>> {
            let mut rows = Vec::new();
            for rel in &p.outputs {
                rows.extend(engine.rows(*rel)?.into_iter().map(|(r, w)| (*rel, r, w)));
            }
            Ok(rows)
        };
        differences(
            &format!("{name}/{i} snapshot"),
            canonical(p, &dd, snapshots(&dd)?)?,
            canonical(p, &duck, snapshots(&duck)?)?,
            &mut errors,
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} row/error differences", errors.len()).into())
    }
}
trait Snapshot {
    fn rows(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError>;
}
impl<E: Engine> Snapshot for E {
    fn rows(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        self.snapshot(rel)
    }
}
fn files(dir: &Path, extension: &str) -> TestResult<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(files(&path, extension)?)
        } else if path.extension().and_then(|s| s.to_str()) == Some(extension) {
            out.push(path)
        }
    }
    out.sort();
    Ok(out)
}
fn report(name: &str, result: TestResult<()>, counts: &mut (usize, usize)) {
    match result {
        Ok(()) => {
            counts.0 += 1;
            println!("CASE PASS {name}")
        }
        Err(e) => {
            counts.1 += 1;
            println!("CASE FAIL {name}: {e}")
        }
    }
}
#[test]
fn shared_conformance() -> TestResult<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let oracle = root.join("ivm-dd/oracle");
    let mut counts = (0, 0);
    for path in files(&oracle, "sql")? {
        let name = path
            .strip_prefix(&oracle)?
            .with_extension("")
            .to_string_lossy()
            .to_string();
        let script = support::script(&name);
        // Some historical headers still name an unsupported operator that DD now supports.
        // Only an actual DD install error selects the expected-rejection path.
        if let Err(expected) = Dd::install(&script.program) {
            let result = match DuckDb::install(&script.program) {
                Err(actual) if expected.kind == actual.kind => Ok(()),
                _ => Err("expected matching install errors".into()),
            };
            report(&name, result, &mut counts);
            continue;
        }
        let (_, steps) = support::oracle(&script);
        if steps.is_empty() {
            println!(
                "CASE NO_FRONTIERS {name}: excluded from script counts; separate lifecycle test"
            );
            continue;
        }
        let frontiers = steps.into_iter().map(|s| s.frontier).collect::<Vec<_>>();
        report(
            &name,
            compare(&name, &script.program, &[], &frontiers),
            &mut counts,
        );
    }
    let corpus = root.join("ivm-sqlite/tests/corpus");
    for path in files(&corpus, "json")? {
        let name = path.strip_prefix(&corpus)?.to_string_lossy().to_string();
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        if name.ends_with("frontiers.json") {
            continue;
        }
        let result = (|| -> TestResult<()> {
            if let Some(p) = json.get("program") {
                let p: Program = serde_json::from_value(p.clone())?;
                let texts: Vec<String> = serde_json::from_value(json["texts"].clone())?;
                let frontiers: Vec<Frontier> =
                    serde_json::from_value(json["dd_frontiers"].clone())?;
                return compare(&name, &p, &texts, &frontiers);
            }
            let p: Program = serde_json::from_value(json)?;
            let frontiers_file = path.with_file_name("frontiers.json");
            let frontiers = if path.file_name().and_then(|s| s.to_str()) == Some("program.json")
                && frontiers_file.exists()
            {
                serde_json::from_str::<Vec<Frontier>>(&std::fs::read_to_string(frontiers_file)?)?
            } else {
                vec![Frontier::default()]
            };
            if frontiers.is_empty() {
                println!("COVERAGE {name}: install only; captured frontier array is empty");
            }
            compare(&name, &p, &[], &frontiers)
        })();
        report(&name, result, &mut counts);
    }
    println!(
        "TOTAL passed={} failed={} cases={}",
        counts.0,
        counts.1,
        counts.0 + counts.1
    );
    if counts.1 > 0 {
        return Err(format!("{} conformance cases failed", counts.1).into());
    }
    Ok(())
}
#[test]
fn string_lifecycle_and_rewind() -> TestResult<()> {
    println!("INSTALL strings");
    let p: Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/16_string.program.json"))?;
    let mut dd = Dd::install(&p)?;
    let mut duck = DuckDb::install(&p)?;
    let steps = [
        (0, "ada", 1),
        (1, "hello", 1),
        (1, "", 1),
        (1, "écho", 1),
        (1, "hello", -1),
        (1, "hello", 1),
        (0, "ada", -1),
        (0, "ada", 1),
        (1, "a\0b\n.print not_a_marker", 1),
    ];
    let mut errors = Vec::new();
    for (i, (rel, text, w)) in steps.into_iter().enumerate() {
        let a = dd.intern_text(text)?;
        let b = duck.intern_text(text)?;
        let a = dd.settle(Frontier {
            changes: vec![SourceChange {
                rel,
                row: vec![a],
                w,
            }],
        })?;
        let b = duck.settle(Frontier {
            changes: vec![SourceChange {
                rel,
                row: vec![b],
                w,
            }],
        })?;
        differences(
            &format!("strings/{i}"),
            canonical(&p, &dd, a.changes)?,
            canonical(&p, &duck, b.changes)?,
            &mut errors,
        );
        println!("WORK strings/{i} {:?}", duck.work());
    }
    let before = duck.snapshot(3)?;
    duck.mark()?;
    let id = duck.intern_text("after mark")?;
    duck.settle(Frontier {
        changes: vec![SourceChange {
            rel: 1,
            row: vec![id],
            w: 1,
        }],
    })?;
    println!("WORK strings/mark_probe {:?}", duck.work());
    duck.rewind()?;
    if duck.snapshot(3)? != before || duck.text(id)?.is_some() {
        errors.push("rewind state differs".into());
    }
    for error in &errors {
        println!("DIFF {error}");
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} string/rewind differences", errors.len()).into())
    }
}
