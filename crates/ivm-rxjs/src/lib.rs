#[path = "2_emit.rs"]
mod emitter;

pub use emitter::emit;

#[cfg(test)]
mod tests {
    use super::*;
    use ivm_ir::*;

    #[test]
    fn emits_per_node_observables() {
        let program = Program {
            terms: vec![],
            texts: vec![],
            rels: vec![
                Relation { id: 0, name: "source".into(), cols: vec![Ty::Int], kind: RelKind::Source },
                Relation { id: 1, name: "output".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
            ],
            nodes: vec![
                Op::Get(0),
                Op::Mfp { input: 0, filter: vec![Expr::Call(Func::Gt, vec![Expr::Col(0), Expr::Lit(2)])], map: vec![], project: vec![] },
                Op::Union(vec![0, 1]),
            ],
            strata: vec![Stratum::Let { id: 1, body: 2 }],
            outputs: vec![1],
        };
        let ts = emit(&program).unwrap();
        let declarations = ts.lines().filter(|line| line.starts_with("  const n") && line.contains(": Observable<Batch> = "))
            .map(|line| line.split(": Observable<Batch> = ").next().unwrap())
            .collect::<Vec<_>>().join("\n");
        assert_eq!(declarations, "  const n0\n  const n1\n  const n2");
        assert!(ts.contains("const n1: Observable<Batch> = n0.pipe(map("));
        assert!(ts.contains("const n2: Observable<Batch> = merge(n0, n1).pipe(scan("));
        assert!(!ts.contains(".subscribe("));
        assert!(!ts.contains("const program:"));
    }

    #[test]
    fn delay_is_unsupported() {
        let program = Program { terms: vec![], texts: vec![], rels: vec![], nodes: vec![Op::Delay(0)], strata: vec![], outputs: vec![] };
        assert!(emit(&program).is_err());
    }
}
