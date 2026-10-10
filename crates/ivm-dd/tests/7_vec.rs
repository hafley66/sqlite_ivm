//! `Op::Vec` on DD: build a flat vec from each source row, then read it back by iter, len and get.

use ivm_dd::{Dd, Engine, Expr, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty, VecOp};

/// `src(a, b, c)`; `items(index, item)`, `len(len)`, `second(item)` over the vec `[a, b, c]`.
fn program() -> Program {
    let relation = |id, name: &str, cols: Vec<Ty>, kind| Relation { id, name: name.into(), cols, kind };
    Program {
        terms: vec![],
        texts: Vec::new(),
        rels: vec![
            relation(0, "src", vec![Ty::Int, Ty::Int, Ty::Int], RelKind::Source),
            relation(1, "items", vec![Ty::Int, Ty::Id], RelKind::Derived),
            relation(2, "len", vec![Ty::Int], RelKind::Derived),
            relation(3, "second", vec![Ty::Id], RelKind::Derived),
        ],
        nodes: vec![
            Op::Get(0),
            // [a, b, c, vec]
            Op::Vec { input: 0, op: VecOp::Build, args: vec![0, 1, 2] },
            // [a, b, c, vec, index, item]
            Op::Vec { input: 1, op: VecOp::Iter, args: vec![3] },
            Op::Mfp { input: 2, filter: vec![], map: vec![], project: vec![4, 5] },
            // [a, b, c, vec, len]
            Op::Vec { input: 1, op: VecOp::Len, args: vec![3] },
            Op::Mfp { input: 4, filter: vec![], map: vec![], project: vec![4] },
            // [a, b, c, vec, 1, item]
            Op::Mfp { input: 1, filter: vec![], map: vec![Expr::Lit(1)], project: vec![] },
            Op::Vec { input: 6, op: VecOp::Get, args: vec![3, 4] },
            Op::Mfp { input: 7, filter: vec![], map: vec![], project: vec![5] },
        ],
        strata: vec![
            Stratum::Let { id: 1, body: 3 },
            Stratum::Let { id: 2, body: 5 },
            Stratum::Let { id: 3, body: 8 },
        ],
        outputs: vec![1, 2, 3],
    }
}

fn sorted(engine: &Dd, rel: u32) -> Vec<(Vec<i64>, i64)> {
    let mut rows = engine.snapshot(rel).unwrap();
    rows.sort();
    rows
}

#[test]
fn a_built_vec_reads_back_by_iter_len_and_get() {
    let mut engine = Dd::install(&program()).unwrap();
    let changes = [[10, 20, 30], [5, 6, 7]]
        .into_iter()
        .map(|row| SourceChange { rel: 0, row: row.to_vec(), w: 1 })
        .collect();
    engine.settle(Frontier { changes }).unwrap();
    assert_eq!(
        sorted(&engine, 1),
        vec![
            (vec![0, 5], 1),
            (vec![0, 10], 1),
            (vec![1, 6], 1),
            (vec![1, 20], 1),
            (vec![2, 7], 1),
            (vec![2, 30], 1),
        ]
    );
    assert_eq!(sorted(&engine, 2), vec![(vec![3], 2)]);
    assert_eq!(sorted(&engine, 3), vec![(vec![6], 1), (vec![20], 1)]);
}

#[test]
fn equal_vecs_share_one_id_and_a_batch_mints_them() {
    let mut engine = Dd::install(&program()).unwrap();
    let one = engine.intern_term_batch(&[
        (ivm_dd::VEC_FUNCTOR, vec![ivm_dd::BatchArg::Cell(1), ivm_dd::BatchArg::Cell(2)]),
        (ivm_dd::VEC_FUNCTOR, vec![ivm_dd::BatchArg::Cell(1), ivm_dd::BatchArg::Cell(2)]),
        (ivm_dd::VEC_FUNCTOR, vec![ivm_dd::BatchArg::Earlier(0)]),
    ]).unwrap();
    assert_eq!(one[0], one[1], "same items, same vec");
    assert_eq!(engine.vec_items(one[0]).unwrap(), Some(vec![1, 2]));
    assert_eq!(engine.vec_items(one[2]).unwrap(), Some(vec![one[0]]), "a vec holding a vec");
    assert_eq!(engine.vec_items(12345).unwrap(), None);
}
