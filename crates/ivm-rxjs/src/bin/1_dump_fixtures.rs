use ivm_dd::{Dd, Engine, Frontier, Op, Program, Raw, RelKind, Relation, SourceChange, Stratum, Ty};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "../../../ivm-dd/tests/support/mod.rs"]
mod support;
#[allow(dead_code)]
#[path = "../../../ivm-dd/tests/random/0_rng.rs"]
mod rng;
#[allow(dead_code)]
#[path = "../../../ivm-dd/tests/random/1_gen.rs"]
mod gen;

fn names(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            names(&path, &format!("{}{}/", prefix, entry.file_name().to_string_lossy()), out);
        } else if path.extension().is_some_and(|e| e == "sql") {
            out.push(format!("{}{}", prefix, path.file_stem().unwrap().to_string_lossy()));
        }
    }
}

fn dump(dir: &Path, name: &str, program: Program, frontiers: Vec<Frontier>) -> Value {
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut dd = <Dd as Engine>::install(&program, &mut host).unwrap();
    let mut expected = Vec::new();
    let mut values = Vec::new();
    for frontier in &frontiers {
        let delta = dd.settle(frontier.clone(), &mut host).unwrap();
        for (rel, row, _) in &delta.changes {
            let types = &program.rel(*rel).unwrap().cols;
            for (cell, ty) in row.iter().zip(types) {
                if *ty == Ty::Id { values.push(*cell); }
            }
        }
        expected.push(delta.changes);
    }
    let mut constructors = BTreeMap::new();
    for rel in program.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
        for (row, _) in dd.intern_snapshot(rel.id, &mut host).unwrap() {
            for (cell, ty) in row[1..].iter().zip(&rel.cols[1..]) {
                if *ty == Ty::Id { values.push(*cell); }
            }
            constructors.insert(row[0], json!({ "functor": rel.name, "args": &row[1..], "types": &rel.cols[1..] }));
        }
    }
    let mut texts = BTreeMap::new();
    for id in values {
        if let Some(value) = dd.text(id, &mut host).unwrap() { texts.insert(id, value); }
    }
    let module = ivm_rxjs::emit(&program).unwrap();
    assert!(!module.contains(".subscribe("));
    fs::write(dir.join(format!("{name}.ts")), module).unwrap();
    json!({ "name": name, "program": program, "frontiers": frontiers, "expected": expected, "constructors": constructors, "texts": texts })
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../target/ivm-rxjs-fixtures");
    fs::create_dir_all(&dir).unwrap();
    let mut all = Vec::new();
    let mut scripts = Vec::new();
    names(&root.join("../ivm-dd/oracle"), "", &mut scripts);
    scripts.sort();
    for name in scripts {
        let script = support::script(&name);
        if script.program.nodes.iter().any(|op| matches!(op, Op::Delay(_))) {
            assert!(ivm_rxjs::emit(&script.program).is_err());
            let db = Connection::open_in_memory().unwrap();
            assert!(<Dd as Engine>::install(&script.program, &mut Raw::with_connection(&db)).is_err());
            continue;
        }
        let (_, steps) = support::oracle(&script);
        let frontiers = steps.into_iter().filter(|step| step.expect_error.is_none()).map(|step| step.frontier).collect();
        let file = name.replace('/', "_");
        all.push(dump(&dir, &file, script.program, frontiers));
    }
    let script_count = all.len();
    for seed in 0..200 {
        let mut rng = rng::Rng(seed);
        let program = gen::program(&mut rng);
        let frontiers = gen::frontiers(&mut rng, &program);
        all.push(dump(&dir, &format!("random_{seed}"), program, frontiers));
    }
    let negate = Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "source".into(), cols: vec![Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "negative".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::Negate(0)],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    };
    let frontiers = vec![
        Frontier { changes: vec![SourceChange { rel: 0, row: vec![4], w: 1 }] },
        Frontier { changes: vec![SourceChange { rel: 0, row: vec![4], w: -1 }] },
    ];
    all.push(dump(&dir, "negate", negate, frontiers));
    let shape = Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "source".into(), cols: vec![Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "output".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mfp { input: 0, filter: vec![ivm_dd::Expr::Call(ivm_dd::Func::Gt, vec![ivm_dd::Expr::Col(0), ivm_dd::Expr::Lit(2)])], map: vec![], project: vec![] },
            Op::Union(vec![0, 1]),
        ],
        strata: vec![Stratum::Let { id: 1, body: 2 }],
        outputs: vec![1],
    };
    fs::write(dir.join("shape_3.ts"), ivm_rxjs::emit(&shape).unwrap()).unwrap();
    let mut counts = BTreeMap::<String, usize>::new();
    for case in &all {
        for op in case["program"]["nodes"].as_array().unwrap() {
            let name = op.as_object().unwrap().keys().next().unwrap().clone();
            *counts.entry(name).or_default() += 1;
        }
        for stratum in case["program"]["strata"].as_array().unwrap() {
            if stratum.get("LetRec").is_some() { *counts.entry("LetRec".into()).or_default() += 1; }
        }
    }
    fs::write(dir.join("cases.json"), serde_json::to_vec(&all).unwrap()).unwrap();
    println!("scripts={script_count} random=200 cases={} op_counts={counts:?}", all.len());
}
