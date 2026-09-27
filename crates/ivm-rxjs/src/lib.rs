use ivm_ir::{Op, Program, Stratum};

/// Emit one self-contained, strict TypeScript module for a checked IR program.
pub fn emit(program: &Program) -> Result<String, String> {
    for op in &program.nodes {
        if matches!(op, Op::Delay(_)) {
            return Err("Delay requires a clock checker".into());
        }
        if let Op::Join { inputs, .. } = op {
            if inputs.len() != 2 { return Err("Join arity != 2".into()); }
        }
    }
    for stratum in &program.strata {
        if let Stratum::LetRec(rec) = stratum {
            if rec.limit.is_some() { return Err("LetRec limit unsupported".into()); }
        }
    }
    let json = serde_json::to_string(program).map_err(|e| e.to_string())?;
    Ok(format!("import {{ EMPTY, Subject, defer, expand, forkJoin, from, last, map, merge, mergeMap, mergeScan, of, reduce, scan, shareReplay }} from 'rxjs';\nconst program: Program = {json};\n{}", include_str!("1_runtime.ts")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_module_without_subscription() {
        let p = Program { texts: vec![], rels: vec![], nodes: vec![], strata: vec![], outputs: vec![] };
        let ts = emit(&p).unwrap();
        assert!(ts.contains("mergeScan"));
        assert!(!ts.contains(".subscribe("));
    }

    #[test]
    fn delay_is_unsupported() {
        let p = Program { texts: vec![], rels: vec![], nodes: vec![Op::Delay(0)], strata: vec![], outputs: vec![] };
        assert!(emit(&p).is_err());
    }
}
