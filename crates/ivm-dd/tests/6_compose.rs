//! `ivm_ir::compose` and `Engine::declare_constructors` on both engines.

use ivm_dd::{compose, Dd, Engine, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;

/// `out(x) :- src(x), x > floor`, with a constructor `9:1:f` the program names.
fn part(floor: i64) -> Program {
    Program {
        terms: vec![],
        texts: Vec::new(),
        rels: vec![
            Relation { id: 0, name: "src".into(), cols: vec![Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "9:1:f".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Constructor },
            Relation { id: 2, name: "out".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mfp {
                input: 0,
                filter: vec![ivm_dd::Expr::Call(ivm_dd::Func::Gt, vec![ivm_dd::Expr::Col(0), ivm_dd::Expr::Lit(floor)])],
                map: vec![],
                project: vec![],
            },
        ],
        strata: vec![Stratum::Let { id: 2, body: 1 }],
        outputs: vec![2],
    }
}

fn composed<E: Engine>() -> (Vec<Vec<(Vec<i64>, i64)>>, Vec<u32>, bool) {
    let (a, b) = (part(0), part(5));
    let composed = compose(&[("a.", &a), ("b.", &b)]).unwrap();
    let mut engine = E::install(&composed.program).unwrap();
    let (src_a, src_b) = (composed.rels[0][&0], composed.rels[1][&0]);
    let changes = [3, 7].iter().map(|x| SourceChange { rel: src_a, row: vec![*x], w: 1 })
        .chain([4, 9].iter().map(|x| SourceChange { rel: src_b, row: vec![*x], w: 1 }))
        .collect();
    engine.settle(Frontier { changes }).unwrap();
    let outs = [composed.rels[0][&2], composed.rels[1][&2]].iter().map(|rel| {
        let mut rows = engine.snapshot(*rel).unwrap();
        rows.sort();
        rows
    }).collect();
    let declared = engine.declare_constructors(&[
        ("9:1:g".to_string(), vec![Ty::Id]),
        ("9:1:f".to_string(), vec![Ty::Id]),
    ]).unwrap();
    let text = engine.intern_text("x").unwrap();
    let first = engine.intern_terms(&[(declared[0], vec![text])]).unwrap();
    let again = engine.intern_terms(&[(declared[0], vec![text])]).unwrap();
    (outs, declared, first == again)
}

#[test]
fn composed_parts_settle_apart_and_declared_constructors_intern() {
    for (outs, declared, stable) in [composed::<Dd>(), composed::<Sqlite>()] {
        assert_eq!(outs, vec![vec![(vec![3], 1), (vec![7], 1)], vec![(vec![9], 1)]]);
        assert_eq!(declared[1], 2, "a constructor the program names keeps its id");
        assert!(declared[0] > 5, "a new constructor gets an id no program relation holds");
        assert!(stable, "one term, one cell");
    }
}
