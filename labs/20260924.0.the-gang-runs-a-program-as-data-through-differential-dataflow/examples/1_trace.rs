//! `cargo run --example 1_trace --features sqlite -- <script under oracle/, e.g. pokemon/0_can_surf> <out.json>`.
//! Trace JSON of plans/2026-09-25-pokemon-trace-demo.md; any engine/oracle/non-loop node disagreement exits 1, writes nothing.

#[cfg(feature = "sqlite")]
#[path = "../tests/support/mod.rs"]
mod support;

#[cfg(not(feature = "sqlite"))]
fn main() {
    eprintln!("1_trace needs --features sqlite");
    std::process::exit(2);
}

#[cfg(feature = "sqlite")]
fn main() {
    trace::main()
}

#[cfg(feature = "sqlite")]
mod trace {
    use super::support;
    use lab_20260924_0::{Agg, DdTap, Dd, Expr, Func, NodeId, Op, Program, RelId, RelKind, Row, Sql, SqlSeen, Stratum, W};
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Serialize, Clone, PartialEq, Debug)]
    struct Change {
        row: Row,
        w: W,
    }

    #[derive(Serialize, PartialEq, Debug)]
    struct RelChange {
        relation: RelId,
        row: Row,
        w: W,
    }

    #[derive(Serialize)]
    struct RelationOut {
        id: RelId,
        name: String,
        kind: &'static str,
        columns: Vec<String>,
    }

    #[derive(Serialize)]
    struct NodeOut {
        id: NodeId,
        op: String,
        detail: serde_json::Value,
        inputs: Vec<NodeId>,
        relation: Option<RelId>,
        caption: String,
        columns: Vec<String>,
        in_loop: bool,
        sqlite_tables: Vec<String>,
        sqlite_fill: Vec<String>,
    }

    #[derive(Serialize)]
    struct Round {
        round: u64,
        row: Row,
        w: W,
    }

    #[derive(Serialize)]
    struct Term {
        label: String,
        changes: Vec<Change>,
    }

    #[derive(Serialize)]
    struct StepNode {
        id: NodeId,
        dd_changes: Vec<Change>,
        dd_rounds: Vec<Round>,
        sqlite_changes: Vec<Change>,
        totals_before: Vec<Change>,
        terms: Vec<Term>,
    }

    #[derive(Serialize)]
    struct StepOut {
        index: usize,
        caption: String,
        frontier: Vec<RelChange>,
        error: Option<String>,
        oracle: Vec<RelChange>,
        dd: Vec<RelChange>,
        sqlite: Vec<RelChange>,
        nodes: Vec<StepNode>,
    }

    #[derive(Serialize)]
    struct Trace {
        scenario: String,
        title: String,
        question: String,
        names: BTreeMap<String, String>,
        relations: Vec<RelationOut>,
        nodes: Vec<NodeOut>,
        steps: Vec<StepOut>,
    }

    fn inputs(op: &Op) -> Vec<NodeId> {
        match op {
            Op::Get(_) => vec![],
            Op::Mfp { input, .. } | Op::Reduce { input, .. } | Op::TopK { input, .. } | Op::Window { input, .. } => vec![*input],
            Op::Union(ns) | Op::Join { inputs: ns, .. } => ns.clone(),
            Op::Negate(n) | Op::Threshold(n) | Op::Delay(n) => vec![*n],
            Op::Antijoin { l, r, .. } => vec![*l, *r],
        }
    }

    fn expr_name(e: &Expr, cols: &[String]) -> String {
        let arg = |e: &Expr| match e {
            Expr::Call(..) => format!("({})", expr_name(e, cols)),
            _ => expr_name(e, cols),
        };
        match e {
            Expr::Col(c) => cols.get(*c as usize).cloned().unwrap_or_else(|| format!("c{c}")),
            Expr::Lit(v) => v.to_string(),
            Expr::Call(Func::Not, args) => format!("not {}", arg(&args[0])),
            Expr::Call(f, args) => {
                let sym = match f {
                    Func::Eq => "=",
                    Func::Ne => "<>",
                    Func::Lt => "<",
                    Func::Le => "<=",
                    Func::Gt => ">",
                    Func::Ge => ">=",
                    Func::Add => "+",
                    Func::Sub => "-",
                    Func::And => "and",
                    Func::Or => "or",
                    Func::Not => unreachable!(),
                };
                format!("{} {sym} {}", arg(&args[0]), arg(&args[1]))
            }
        }
    }

    fn rel_columns(conn: &rusqlite::Connection, p: &Program, rel: RelId) -> Vec<String> {
        let r = p.rel(rel).unwrap();
        let cols = support::columns(conn, &r.name);
        if cols.len() == r.cols.len() {
            cols
        } else {
            (0..r.cols.len()).map(|c| format!("c{c}")).collect()
        }
    }

    fn columns(conn: &rusqlite::Connection, p: &Program, id: NodeId, memo: &mut BTreeMap<NodeId, Vec<String>>) -> Vec<String> {
        if let Some(c) = memo.get(&id) {
            return c.clone();
        }
        let mut of = |n: NodeId| columns(conn, p, n, memo);
        let cols = match &p.nodes[id as usize] {
            Op::Get(rel) => rel_columns(conn, p, *rel),
            Op::Mfp { input, map, project, .. } => {
                let mut cols = of(*input);
                for e in map {
                    let name = expr_name(e, &cols);
                    cols.push(name);
                }
                if project.is_empty() { cols } else { project.iter().map(|c| cols[*c as usize].clone()).collect() }
            }
            Op::Join { inputs, .. } => inputs.iter().flat_map(|n| of(*n)).collect(),
            Op::Union(ns) => of(ns[0]),
            Op::Negate(n) | Op::Threshold(n) | Op::Delay(n) => of(*n),
            Op::TopK { input, .. } | Op::Window { input, .. } => of(*input),
            Op::Antijoin { l, .. } => of(*l),
            Op::Reduce { input, key, aggs } => {
                let cols = of(*input);
                let mut out: Vec<String> = key.iter().map(|k| cols[*k as usize].clone()).collect();
                out.extend(aggs.iter().map(|a| match a {
                    Agg::Count => "count".to_string(),
                    Agg::Sum(c) => format!("sum of {}", cols[*c as usize]),
                    Agg::Min(c) => format!("min {}", cols[*c as usize]),
                    Agg::Max(c) => format!("max {}", cols[*c as usize]),
                }));
                out
            }
        };
        memo.insert(id, cols.clone());
        cols
    }

    fn changes(rows: &[(Row, W)]) -> Vec<Change> {
        rows.iter().map(|(row, w)| Change { row: row.clone(), w: *w }).collect()
    }

    fn rel_changes(rows: &[(RelId, Row, W)]) -> Vec<RelChange> {
        rows.iter().map(|(relation, row, w)| RelChange { relation: *relation, row: row.clone(), w: *w }).collect()
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let [name, out] = &args[..] else {
            eprintln!("usage: 1_trace <script under oracle/, e.g. pokemon/0_can_surf> <out.json>");
            std::process::exit(2);
        };
        let script = support::script(name);
        let p = &script.program;
        let (conn, steps) = support::oracle(&script);

        let mut dd = Dd::install_traced(p).unwrap_or_else(|e| fail(&format!("Dd install: {e}")));
        let mut sql = Sql::install_traced(p).unwrap_or_else(|e| fail(&format!("Sql install: {e}")));

        // Per IR node: the SQL tag read for it (an outside lowering wins over one inside a LetRec).
        let mut tag_of: BTreeMap<NodeId, usize> = BTreeMap::new();
        for (i, tag) in sql.tags.iter().enumerate() {
            let better = match tag_of.get(&tag.id) {
                None => true,
                Some(j) => sql.tags[*j].in_loop && !tag.in_loop,
            };
            if better {
                tag_of.insert(tag.id, i);
            }
        }
        let in_loop = |id: NodeId| tag_of.get(&id).is_some_and(|i| sql.tags[*i].in_loop);

        let mut defines: BTreeMap<NodeId, RelId> = BTreeMap::new();
        for stratum in &p.strata {
            match stratum {
                Stratum::Let { id, body } => {
                    defines.insert(*body, *id);
                }
                Stratum::LetRec(rec) => defines.extend(rec.bodies.iter().copied().zip(rec.ids.iter().copied())),
            }
        }
        let relation = |id: NodeId| match &p.nodes[id as usize] {
            Op::Get(rel) => Some(*rel),
            _ => defines.get(&id).copied(),
        };
        let caption = |id: NodeId| script.captions.iter().find(|(n, _)| *n == id).map(|(_, c)| c.clone()).unwrap_or_default();
        let label = |id: NodeId| {
            let c = caption(id);
            if !c.is_empty() {
                return c;
            }
            relation(id).and_then(|r| p.rel(r)).map(|r| r.name.clone()).unwrap_or_else(|| format!("node {id}"))
        };

        let mut memo = BTreeMap::new();
        let relations = p
            .rels
            .iter()
            .map(|r| RelationOut {
                id: r.id,
                name: r.name.clone(),
                kind: if r.kind == RelKind::Source { "Source" } else { "Derived" },
                columns: rel_columns(&conn, p, r.id),
            })
            .collect();
        let nodes: Vec<NodeOut> = p
            .nodes
            .iter()
            .enumerate()
            .map(|(i, op)| {
                let id = i as NodeId;
                let detail = serde_json::to_value(op).unwrap();
                let tag = tag_of.get(&id).map(|t| &sql.tags[*t]);
                NodeOut {
                    id,
                    op: detail.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default(),
                    detail,
                    inputs: inputs(op),
                    relation: relation(id),
                    caption: caption(id),
                    columns: columns(&conn, p, id, &mut memo),
                    in_loop: in_loop(id),
                    sqlite_tables: tag.map(|t| t.tables.clone()).unwrap_or_default(),
                    sqlite_fill: tag.map(|t| t.fills.clone()).unwrap_or_default(),
                }
            })
            .collect();

        let mut mismatches: Vec<String> = Vec::new();
        let mut out_steps = Vec::new();
        for (index, step) in steps.into_iter().enumerate() {
            let at = format!("step {index} ({})", step.caption);
            let frontier: Vec<RelChange> = step.frontier.changes.iter().map(|c| RelChange { relation: c.rel, row: c.row.clone(), w: c.w }).collect();
            let d = dd.settle_traced(step.frontier.clone());
            let s = sql.settle_traced(step.frontier);
            let mut out = StepOut {
                index,
                caption: step.caption,
                frontier,
                error: None,
                oracle: vec![],
                dd: vec![],
                sqlite: vec![],
                nodes: vec![],
            };
            match (&step.expect_error, step.oracle, d, s) {
                (Some(kind), Err(_), Err(de), Err(se)) => {
                    for e in [de, se] {
                        if !format!("{:?}", e.kind).starts_with(kind.as_str()) {
                            mismatches.push(format!("{at}: expected {kind}, got {e}"));
                        }
                    }
                    out.error = Some(kind.clone());
                }
                (Some(kind), oracle, d, s) => mismatches.push(format!(
                    "{at}: expected {kind} from all; oracle {:?}, dd {:?}, sqlite {:?}",
                    oracle.err(),
                    d.err(),
                    s.err()
                )),
                (None, Err(e), _, _) => mismatches.push(format!("{at}: oracle SQL failed: {e}")),
                (None, Ok(_), Err(e), _) | (None, Ok(_), _, Err(e)) => mismatches.push(format!("{at}: engine error {e}")),
                (None, Ok(expected), Ok((dd_delta, taps)), Ok((sql_delta, seen))) => {
                    if dd_delta.changes != expected {
                        mismatches.push(format!("{at}: dd delta {:?} != oracle {:?}", dd_delta.changes, expected));
                    }
                    if sql_delta.changes != expected {
                        mismatches.push(format!("{at}: sqlite delta {:?} != oracle {:?}", sql_delta.changes, expected));
                    }
                    out.oracle = rel_changes(&expected);
                    out.dd = rel_changes(&dd_delta.changes);
                    out.sqlite = rel_changes(&sql_delta.changes);
                    out.nodes = nodes.iter().map(|n| step_node(n, &taps, &seen, &tag_of, p, &label, &at, &mut mismatches)).collect();
                }
            }
            out_steps.push(out);
        }
        if !mismatches.is_empty() {
            for m in &mismatches {
                eprintln!("{name}: {m}");
            }
            std::process::exit(1);
        }
        let trace = Trace {
            scenario: name.rsplit('/').next().unwrap().to_string(),
            title: script.title.clone(),
            question: script.question.clone(),
            names: script.names.iter().cloned().collect(),
            relations,
            nodes,
            steps: out_steps,
        };
        let out = std::path::Path::new(out);
        if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(out, serde_json::to_string_pretty(&trace).unwrap()).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn step_node(
        n: &NodeOut,
        taps: &[DdTap],
        seen: &[SqlSeen],
        tag_of: &BTreeMap<NodeId, usize>,
        p: &Program,
        label: &dyn Fn(NodeId) -> String,
        at: &str,
        mismatches: &mut Vec<String>,
    ) -> StepNode {
        let mine: Vec<&DdTap> = taps.iter().filter(|t| t.node == n.id).collect();
        let outer = mine.iter().any(|t| t.round.is_none());
        let mut net: BTreeMap<Row, W> = BTreeMap::new();
        for t in mine.iter().filter(|t| t.round.is_none() == outer) {
            *net.entry(t.row.clone()).or_default() += t.w;
        }
        let dd_changes: Vec<Change> = net.into_iter().filter(|(_, w)| *w != 0).map(|(row, w)| Change { row, w }).collect();
        let dd_rounds = mine
            .iter()
            .filter_map(|t| t.round.map(|round| Round { round, row: t.row.clone(), w: t.w }))
            .collect();
        let s = tag_of.get(&n.id).map(|i| &seen[*i]);
        let sqlite_changes = s.map(|s| changes(&s.changes)).unwrap_or_default();
        if !n.in_loop && dd_changes != sqlite_changes {
            mismatches.push(format!("{at}: node {} dd {:?} != sqlite {:?}", n.id, dd_changes, sqlite_changes));
        }
        let terms = match (&p.nodes[n.id as usize], s) {
            (Op::Join { inputs, .. }, Some(s)) if inputs.len() == 2 => {
                let (a, b) = (label(inputs[0]), label(inputs[1]));
                let labels = [format!("new {a} × new {b}"), format!("existing {a} × new {b}"), format!("new {a} × existing {b}")];
                labels.into_iter().zip(&s.terms).map(|(label, rows)| Term { label, changes: changes(rows) }).collect()
            }
            _ => vec![],
        };
        StepNode {
            id: n.id,
            dd_changes,
            dd_rounds,
            sqlite_changes,
            totals_before: s.map(|s| changes(&s.totals_before)).unwrap_or_default(),
            terms,
        }
    }

    fn fail(msg: &str) -> ! {
        eprintln!("{msg}");
        std::process::exit(1);
    }
}
