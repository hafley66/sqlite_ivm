use ivm_ir::*;
use serde::Serialize;
use std::fmt::Write;

fn json<T: Serialize + ?Sized>(value: &T) -> String { serde_json::to_string(value).unwrap() }
fn render(template: &str, values: &[(&str, String)]) -> String {
    values.iter().fold(template.to_owned(), |text, (key, value)| text.replace(&format!("@{key}@"), value))
}
fn numbered(id: usize, body: String) -> String {
    format!("  const n{id}: Observable<Batch> = {body}\n")
}
fn cached() -> &'static str { "shareReplay({ bufferSize: 1, refCount: false })" }

fn inputs(op: &Op) -> Vec<NodeId> {
    match op {
        Op::Get(_) => vec![],
        Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
        | Op::Reduce { input, .. } | Op::TopK { input, .. } | Op::Window { input, .. } => vec![*input],
        Op::Union(inputs) | Op::Join { inputs, .. } => inputs.clone(),
        Op::Negate(input) | Op::Threshold(input) | Op::Delay(input) => vec![*input],
        Op::Antijoin { l, r, .. } => vec![*l, *r],
    }
}

fn reachable(program: &Program, roots: &[NodeId]) -> Vec<usize> {
    let mut seen = vec![false; program.nodes.len()];
    let mut pending = roots.to_vec();
    while let Some(id) = pending.pop() {
        let index = id as usize;
        if seen[index] { continue; }
        seen[index] = true;
        pending.extend(inputs(&program.nodes[index]));
    }
    seen.iter().enumerate().filter_map(|(id, used)| used.then_some(id)).collect()
}

/// The first two operands of a call; the rest never render.
fn operands(expr: &Expr) -> &[Expr] {
    match expr {
        Expr::Call(_, args) => &args[..args.len().min(2)],
        _ => &[],
    }
}

/// Post-order with an explicit stack: pre-order with the last operand first, reversed, puts
/// every operand before its call, left to right.
fn expression(expr: &Expr, row: &str) -> String {
    let mut order = Vec::new();
    let mut walk = vec![expr];
    while let Some(next) = walk.pop() {
        order.push(next);
        walk.extend(operands(next));
    }
    let mut done: Vec<String> = Vec::with_capacity(order.len());
    for e in order.into_iter().rev() {
        let rendered = done.split_off(done.len() - operands(e).len());
        done.push(expression_node(e, row, rendered));
    }
    done.pop().unwrap_or_default()
}

fn expression_node(expr: &Expr, row: &str, rendered: Vec<String>) -> String {
    match expr {
        Expr::Col(col) => format!("{row}[{col}]"),
        Expr::Lit(value) => value.to_string(),
        Expr::Text(index) => format!("terms.literals[{index}]"),
        Expr::Term(_) => unreachable!("emit rejects a program with a term table"),
        Expr::Call(func, _) => {
            let mut rendered = rendered.into_iter();
            let a = rendered.next().unwrap_or_default();
            let b = rendered.next().unwrap_or_default();
            match func {
                Func::Eq => format!("+( Number({a}) === Number({b}) )"),
                Func::Ne => format!("+( Number({a}) !== Number({b}) )"),
                Func::Lt => format!("+( {a} < {b} )"),
                Func::Le => format!("+( {a} <= {b} )"),
                Func::Gt => format!("+( {a} > {b} )"),
                Func::Ge => format!("+( {a} >= {b} )"),
                Func::Add => format!("Number(BigInt.asIntN(64, BigInt({a}) + BigInt({b})))"),
                Func::Sub => format!("Number(BigInt.asIntN(64, BigInt({a}) - BigInt({b})))"),
                Func::And => format!("+( Number({a}) !== 0 && Number({b}) !== 0 )"),
                Func::Or => format!("+( Number({a}) !== 0 || Number({b}) !== 0 )"),
                Func::Not => format!("+( Number({a}) === 0 )"),
                Func::TermLt => format!("+( termCompare(terms, {a}, {b}) < 0 )"),
                Func::StrNil => "terms.byText['']".into(),
            }
        }
    }
}

fn node_source(program: &Program, id: usize, op: &Op) -> Result<String, String> {
    let old = format!("oldNode(oldNodes, {id})");
    let body = match op {
        Op::Get(rel) => {
            let relation = program.rel(*rel).ok_or_else(|| format!("unknown relation {rel}"))?;
            let now = if relation.kind == RelKind::Constructor {
                format!("constructorBag(terms, {})", json(&relation.name))
            } else { format!("rels.get({rel}) ?? empty()") };
            render("defer(() => of(0).pipe(map(() => { const old = @OLD@; const now = @NOW@; return pack(@ID@, old, diff(old, now)); }))).pipe(@CACHE@);",
                &[("OLD", old), ("NOW", now), ("ID", id.to_string()), ("CACHE", cached().into())])
        }
        Op::Mint { input, functor, args } => {
            let relation = program.rel(*functor).ok_or_else(|| format!("unknown constructor {functor}"))?;
            render("n@INPUT@.pipe(map(b => { const delta = empty(); for (const { row, w } of b) { const id = mint(terms, @FUNCTOR@, select(row, @ARGS@), @TYPES@); put(delta, [...row, id], w); } return pack(@ID@, @OLD@, delta, b); }), @CACHE@);",
                &[("INPUT", input.to_string()), ("FUNCTOR", json(&relation.name)), ("ARGS", json(args)), ("TYPES", json(&relation.cols[1..])), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())])
        }
        Op::StrCons { input, mode } => {
            let action = match mode {
                StrMode::Construct { head, rest } => format!("const head = terms.byId[row[{head}]]?.text; const rest = terms.byId[row[{rest}]]?.text; if (head !== undefined && rest !== undefined) put(delta, [...row, mintText(terms, head + rest)], w);"),
                StrMode::Decompose { whole } => format!("const split = splitText(terms, row[{whole}]); if (split) put(delta, [...row, ...split], w);"),
            };
            render("n@INPUT@.pipe(map(b => { const delta = empty(); for (const { row, w } of b) { @ACTION@ } return pack(@ID@, @OLD@, delta, b); }), @CACHE@);",
                &[("INPUT", input.to_string()), ("ACTION", action), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())])
        }
        Op::Str { op, .. } => return Err(format!("string op {} has no rxjs emit", op.name())),
        Op::Mfp { input, filter, map, project } => {
            let condition = if filter.is_empty() { "true".into() } else { filter.iter().map(|e| format!("Number({}) !== 0", expression(e, "row"))).collect::<Vec<_>>().join(" && ") };
            let maps = map.iter().map(|e| format!("next.push({});", expression(e, "next"))).collect::<Vec<_>>().join(" ");
            let result = if project.is_empty() { "next".into() } else { format!("select(next, {})", json(project)) };
            render("n@INPUT@.pipe(map(b => { const delta = empty(); for (const { row, w } of b) { if (!(@CONDITION@)) continue; const next = [...row]; @MAPS@ put(delta, @RESULT@, w); } return pack(@ID@, @OLD@, delta, b); }), @CACHE@);",
                &[("INPUT", input.to_string()), ("CONDITION", condition), ("MAPS", maps), ("RESULT", result), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())])
        }
        Op::Union(inputs) => {
            if inputs.is_empty() { return Err("Union has no inputs".into()); }
            let streams = inputs.iter().map(|n| format!("n{n}")).collect::<Vec<_>>().join(", ");
            render("merge(@STREAMS@).pipe(scan((acc, b) => ({ delta: add(copy(acc.delta), rows(b)), children: [...acc.children, b] }), { delta: empty(), children: [] as Batch[] }), last(), map(acc => pack(@ID@, @OLD@, acc.delta, ...acc.children)), @CACHE@);",
                &[("STREAMS", streams), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())])
        }
        Op::Negate(input) => render("n@INPUT@.pipe(map(b => { const delta = empty(); for (const { row, w } of b) put(delta, row, -w); return pack(@ID@, @OLD@, delta, b); }), @CACHE@);",
            &[("INPUT", input.to_string()), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())]),
        Op::Join { inputs, equivalences } => {
            if inputs.len() != 2 { return Err("Join arity != 2".into()); }
            let side = |which: u8| -> Result<Vec<ColId>, String> {
                equivalences.iter().map(|class| class.iter().find(|(i, _)| *i == which).map(|(_, c)| *c).ok_or_else(|| "Join class missing side".into())).collect()
            };
            render(r#"forkJoin([n@LEFT@, n@RIGHT@]).pipe(mergeMap(([a, b]) => merge(of({ side: 0, batch: a }), of({ side: 1, batch: b })).pipe(
              scan((state, event) => {
                const delta = copy(state.delta);
                if (event.side === 0) {
                  let left = state.left;
                  for (const { row, w } of event.batch) {
                    const key = keyOf(row, @LK@);
                    for (const other of state.right.get(key)?.values() ?? []) put(delta, [...row, ...other.row], w * other.w);
                    left = indexAdd(left, key, row, w);
                  }
                  return { left, right: state.right, delta };
                }
                let right = state.right;
                for (const { row, w } of event.batch) {
                  const key = keyOf(row, @RK@);
                  for (const other of state.left.get(key)?.values() ?? []) put(delta, [...other.row, ...row], other.w * w);
                  right = indexAdd(right, key, row, w);
                }
                return { left: state.left, right, delta };
              }, { left: groups(node(a).old, @LK@), right: groups(node(b).old, @RK@), delta: empty() }),
              last(), map(state => pack(@ID@, @OLD@, state.delta, a, b)),
            )), @CACHE@);"#,
                &[("LEFT", inputs[0].to_string()), ("RIGHT", inputs[1].to_string()), ("LK", json(&side(0)?)), ("RK", json(&side(1)?)), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())])
        }
        Op::Antijoin { l, r, lk, rk } => render("forkJoin([n@LEFT@, n@RIGHT@]).pipe(scan((_: Batch | null, [a, b]) => { const delta = empty(); const before = weightByKey(node(b).old, @RK@); const after = weightByKey(node(b).now, @RK@); for (const { row, w } of a) if ((after.get(keyOf(row, @LK@)) ?? 0) <= 0) put(delta, row, w); for (const { row, w } of node(a).old.values()) { const key = keyOf(row, @LK@); const had = (before.get(key) ?? 0) <= 0; const has = (after.get(key) ?? 0) <= 0; if (had !== has) put(delta, row, has ? w : -w); } return pack(@ID@, @OLD@, delta, a, b); }, null), map(b => b!), @CACHE@);",
            &[("LEFT", l.to_string()), ("RIGHT", r.to_string()), ("LK", json(lk)), ("RK", json(rk)), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())]),
        Op::Reduce { input, key, aggs } => reduce_source(program, id, *input, key, aggs),
        Op::Threshold(input) => render("n@INPUT@.pipe(scan((_: Batch | null, b) => { const delta = empty(); for (const { row } of b) { const key = JSON.stringify(row); const before = (node(b).old.get(key)?.w ?? 0) > 0; const after = (node(b).now.get(key)?.w ?? 0) > 0; if (before !== after) put(delta, row, after ? 1 : -1); } return pack(@ID@, @OLD@, delta, b); }, null), map(b => b!), @CACHE@);",
            &[("INPUT", input.to_string()), ("ID", id.to_string()), ("OLD", old), ("CACHE", cached().into())]),
        Op::TopK { input, key, order, limit } => topk_source(program, id, *input, key, order, *limit),
        Op::Window { input, partition, order, func } => window_source(program, id, *input, partition, order, func),
        Op::Delay(_) => return Err("Delay requires a clock checker".into()),
    };
    Ok(numbered(id, body))
}

fn reduce_source(program: &Program, id: usize, input: NodeId, key: &[ColId], aggs: &[Agg]) -> String {
    let types = program.node_types(input).unwrap();
    let values = aggs.iter().map(|agg| match agg {
        Agg::Count => "count".into(),
        Agg::Sum(col) => format!("members.reduce((n, item) => n + item.row[{col}] * item.w, 0)"),
        Agg::Min(col) | Agg::Max(col) => {
            let direction = if matches!(agg, Agg::Min(_)) { "[0]" } else { "[live.length - 1]" };
            format!("live.map(item => item.row[{col}]).sort((a, b) => cmpCell({}, a, b, terms)){direction}", json(&types[*col as usize]))
        }
    }).collect::<Vec<_>>().join(", ");
    render("n@INPUT@.pipe(scan((_: Batch | null, b) => { const delta = empty(); const before = groups(node(b).old, @KEY@); const after = groups(node(b).now, @KEY@); const make = (key: string, bag: Bag): Bag => { const out = empty(); const members = [...bag.values()]; const count = members.reduce((n, item) => n + item.w, 0); if (count <= 0) return out; const live = members.filter(item => item.w > 0); const values = [@VALUES@]; put(out, [...JSON.parse(key) as Row, ...values], 1); return out; }; for (const key of new Set(b.map(change => keyOf(change.row, @KEY@)))) add(delta, diff(make(key, before.get(key) ?? empty()), make(key, after.get(key) ?? empty()))); return pack(@ID@, @OLD@, delta, b); }, null), map(b => b!), @CACHE@);",
        &[("INPUT", input.to_string()), ("KEY", json(&key)), ("VALUES", values), ("ID", id.to_string()), ("OLD", format!("oldNode(oldNodes, {id})")), ("CACHE", cached().into())])
}

fn topk_source(program: &Program, id: usize, input: NodeId, key: &[ColId], order: &[Order], limit: u32) -> String {
    render("n@INPUT@.pipe(scan((_: Batch | null, b) => { const delta = empty(); const before = groups(node(b).old, @KEY@); const after = groups(node(b).now, @KEY@); const choose = (bag: Bag): Bag => { const out = empty(); const live = [...bag.values()].filter(item => item.w > 0); live.sort((a, b) => rank(a.row, b.row, @ORDER@, @TYPES@, terms)); let left = @LIMIT@; for (const { row, w } of live) { if (!left) break; const take = Math.min(w, left); put(out, row, take); left -= take; } return out; }; for (const key of new Set(b.map(change => keyOf(change.row, @KEY@)))) add(delta, diff(choose(before.get(key) ?? empty()), choose(after.get(key) ?? empty()))); return pack(@ID@, @OLD@, delta, b); }, null), map(b => b!), @CACHE@);",
        &[("INPUT", input.to_string()), ("KEY", json(&key)), ("ORDER", json(&order)), ("TYPES", json(&program.node_types(input).unwrap())), ("LIMIT", limit.to_string()), ("ID", id.to_string()), ("OLD", format!("oldNode(oldNodes, {id})")), ("CACHE", cached().into())])
}

fn window_source(program: &Program, id: usize, input: NodeId, partition: &[ColId], order: &[Order], func: &WinFn) -> String {
    let value_col = order.first().map_or(0, |entry| entry.col);
    let total_sum = match func {
        WinFn::Sum(col) => format!("rows.reduce((n, row) => n + row[{col}], 0)"),
        _ => "0".into(),
    };
    let value = match func {
        WinFn::RowNumber => "i + 1".into(),
        WinFn::Rank => "rankAt".into(),
        WinFn::DenseRank => "dense".into(),
        WinFn::Lag(offset) => format!("rows[i - {offset}]?.[{value_col}] ?? 0"),
        WinFn::Lead(offset) => format!("rows[i + {offset}]?.[{value_col}] ?? 0"),
        WinFn::Sum(col) => format!("(prefixSum += row[{col}], @ORDER@.length ? prefixSum : totalSum)"),
        WinFn::Count => "@ORDER@.length ? i + 1 : rows.length".into(),
    };
    let value = value.replace("@ORDER@", &json(&order));
    render("n@INPUT@.pipe(scan((_: Batch | null, b) => { const delta = empty(); const before = groups(node(b).old, @PARTITION@); const after = groups(node(b).now, @PARTITION@); const window = (bag: Bag): Bag => { const out = empty(); const rows = [...bag.values()].flatMap(({ row, w }) => Array.from({ length: Math.max(0, w) }, () => row)); rows.sort((a, b) => rank(a, b, @ORDER@, @TYPES@, terms)); const totalSum = @TOTAL@; let dense = 0, rankAt = 0, prefixSum = 0; rows.forEach((row, i) => { if (i === 0 || @ORDER@.some(entry => row[entry.col] !== rows[i - 1][entry.col])) { dense++; rankAt = i + 1; } const value = @VALUE@; put(out, [...row, value], 1); }); return out; }; for (const key of new Set(b.map(change => keyOf(change.row, @PARTITION@)))) add(delta, diff(window(before.get(key) ?? empty()), window(after.get(key) ?? empty()))); return pack(@ID@, @OLD@, delta, b); }, null), map(b => b!), @CACHE@);",
        &[("INPUT", input.to_string()), ("PARTITION", json(&partition)), ("ORDER", json(&order)), ("TYPES", json(&program.node_types(input).unwrap())), ("TOTAL", total_sum), ("VALUE", value), ("ID", id.to_string()), ("OLD", format!("oldNode(oldNodes, {id})")), ("CACHE", cached().into())])
}

fn stratum_source(stratum: &Stratum, index: usize) -> Result<String, String> {
    match stratum {
        // A node can be read before and after a mint in different strata. Rebuild
        // from the current relations so a later read cannot hide its next delta.
        Stratum::Let { id, body } => Ok(render("    concatMap(frame => { const nodes = compute@INDEX@(frame.rels, new Map(), frame.terms); return nodes.n@BODY@.pipe(map(batch => { const rels = new Map(frame.rels); rels.set(@REL@, node(batch).now); const nextNodes = new Map(frame.nodes); for (const [id, bag] of node(batch).nodes) nextNodes.set(id, bag); return { rels, nodes: nextNodes, terms: frame.terms }; })); }),\n",
            &[("BODY", body.to_string()), ("REL", id.to_string()), ("INDEX", index.to_string())])),
        Stratum::LetRec(rec) => {
            if rec.limit.is_some() { return Err("LetRec limit unsupported".into()); }
            if !rec.nested.is_empty() { return Err("nested LetRec unsupported".into()); }
            if rec.ids.len() != rec.bodies.len() || rec.ids.is_empty() { return Err("LetRec ids and bodies mismatch".into()); }
            let mut next = String::new();
            for (i, rel) in rec.ids.iter().enumerate() {
                writeln!(next, "        const body{i} = empty(); for (const {{ row, w }} of node(batches[{i}]).now.values()) if (w > 0) put(body{i}, row, 1); rels.set({rel}, body{i});").unwrap();
            }
            let bodies = rec.bodies.iter().map(|id| format!("nodes.n{id}")).collect::<Vec<_>>().join(", ");
            Ok(render(r#"    concatMap(frame => defer(() => {
      type Round = { rels: Map<number, Bag>; delta: Map<number, Bag>; changed: boolean; n: number };
      const first = new Map(frame.rels);
      const firstDelta = new Map<number, Bag>();
      for (const id of @IDS@) { first.set(id, empty()); firstDelta.set(id, diff(previous.rels.get(id) ?? empty(), empty())); }
      const initial: Round = { rels: first, delta: firstDelta, changed: true, n: 0 };
      const next = (round: Round): Observable<Round> => {
        if (round.n > 1000) throw new Error('LetRec did not converge');
        const nodes = compute@INDEX@(round.rels, new Map(), frame.terms);
        return forkJoin([@BODIES@]).pipe(map(batches => {
          const rels = new Map(round.rels);
@NEXT@          const delta = new Map<number, Bag>();
          for (const id of @IDS@) delta.set(id, diff(round.rels.get(id) ?? empty(), rels.get(id) ?? empty()));
          return { rels, delta, changed: [...delta.values()].some(bag => bag.size > 0), n: round.n + 1 };
        }));
      };
      return of(initial).pipe(
        expand(round => round.changed ? next(round) : EMPTY),
        reduce((acc, round) => {
          const net = new Map(acc.net);
          for (const id of @IDS@) net.set(id, add(copy(net.get(id) ?? empty()), round.delta.get(id) ?? empty()));
          return { net };
        }, { net: new Map<number, Bag>() }),
        map(({ net }) => {
          const rels = new Map(frame.rels);
          for (const id of @IDS@) rels.set(id, add(copy(previous.rels.get(id) ?? empty()), net.get(id) ?? empty()));
          return { rels, nodes: frame.nodes, terms: frame.terms };
        }),
      );
    })),
"#,
                &[("IDS", json(&rec.ids)), ("BODIES", bodies), ("NEXT", next), ("INDEX", index.to_string())]))
        }
    }
}

pub fn emit(program: &Program) -> Result<String, String> {
    for op in &program.nodes {
        if matches!(op, Op::Delay(_)) { return Err("Delay requires a clock checker".into()); }
    }
    if !program.terms.is_empty() {
        return Err("a term table: resolve each Expr::Term to its cell before emitting".into());
    }
    let source_ids: Vec<_> = program.rels.iter().filter(|rel| rel.kind == RelKind::Source).map(|rel| rel.id).collect();
    let mut out = String::new();
    out.push_str("import { EMPTY, Subject, defer, expand, forkJoin, last, map, merge, mergeMap, mergeScan, of, reduce, scan, shareReplay, concatMap } from 'rxjs';\nimport type { Observable } from 'rxjs';\n");
    out.push_str(include_str!("1_runtime.ts"));
    writeln!(out, "\nconst sourceIds = {};", json(&source_ids)).unwrap();
    writeln!(out, "const texts: string[] = {};", json(&program.texts)).unwrap();
    writeln!(out, "const usesStrings = {};", program.uses_strings()).unwrap();
    for (index, stratum) in program.strata.iter().enumerate() {
        let roots = match stratum {
            Stratum::Let { body, .. } => vec![*body],
            Stratum::LetRec(rec) => rec.bodies.clone(),
        };
        let used = reachable(program, &roots);
        writeln!(out, "\nfunction compute{index}(rels: Map<number, Bag>, oldNodes: Map<number, Bag>, terms: Terms) {{").unwrap();
        for id in used {
            out.push_str(&node_source(program, id, &program.nodes[id])?);
        }
        writeln!(out, "  return {{ {} }};\n}}", roots.iter().map(|id| format!("n{id}")).collect::<Vec<_>>().join(", ")).unwrap();
    }
    out.push_str("\nfunction strata$(frame: Frame, previous: State): Observable<Frame> {\n  let frames$: Observable<Frame> = of(frame);\n");
    for (index, stratum) in program.strata.iter().enumerate() {
        let operator = stratum_source(stratum, index)?;
        writeln!(out, "  frames$ = frames$.pipe({});", operator.trim().trim_end_matches(',')).unwrap();
    }
    out.push_str("  return frames$;\n}\n");
    out.push_str("\nfunction initialState(): State {\n  const rels = new Map<number, Bag>();\n  for (const id of sourceIds) rels.set(id, empty());\n  return { rels, nodes: new Map(), terms: initialTerms(texts, usesStrings), outputs: new Map(), changes: [] };\n}\n");
    out.push_str("\nexport function run(frontiers$: Observable<Frontier>): Observable<Batch> {\n  return defer(() => frontiers$.pipe(\n    mergeScan((state: State, frontier) => {\n      if (frontier.changes.length === 0) return of({ ...state, changes: [] });\n      const rels = sourceFrontier(state.rels, frontier, sourceIds);\n      const terms = structuredClone(state.terms);\n      const first = { frame: { rels, nodes: new Map<number, Bag>(), terms }, old: state, before: terms.next, pass: 0 };\n      return of(first).pipe(\n        expand(round => {\n          if (round.pass && round.frame.terms.next === round.before && (round.pass === 1 || sameRels(round.old.rels, round.frame.rels))) return EMPTY;\n          if (round.pass >= 8192) throw new Error('constructor closure budget');\n          const before = round.frame.terms.next;\n          return strata$(round.frame, round.old).pipe(map(frame => ({\n            frame, old: { ...round.old, rels: frame.rels, nodes: frame.nodes }, before, pass: round.pass + 1,\n          })));\n        }),\n        last(),\n        map(({ frame }) => {\n        const outputs = new Map<number, Bag>();\n        const changes: Change[] = [];\n");
    for rel in &program.outputs {
        writeln!(out, "        {{ const now = frame.rels.get({rel}) ?? empty(); outputs.set({rel}, now); for (const {{ row, w }} of diff(state.outputs.get({rel}) ?? empty(), now).values()) changes.push({{ rel: {rel}, row, w }}); }}").unwrap();
    }
    out.push_str("        changes.sort((a, b) => a.rel - b.rel || a.row.reduce((order, value, i) => order || value - b.row[i], 0));\n        return { rels: frame.rels, nodes: frame.nodes, terms: frame.terms, outputs, changes };\n      }));\n    }, initialState(), 1),\n    map(state => { const batch = state.changes as Batch; Object.defineProperty(batch, dictionary, { value: state.terms }); return batch; }),\n  ));\n}\n\nexport const input = new Subject<Frontier>();\nexport const outputs$ = run(input.asObservable());\n");
    Ok(out)
}
