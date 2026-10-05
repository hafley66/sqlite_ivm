use crate::{
    sql,
    transport::{error, literal, Sql, Work},
};
use ivm_engine::{Counters, Engine, EngineError, ErrorKind, Stage};
use ivm_ir::*;
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

type Result<T> = std::result::Result<T, EngineError>;

pub struct DuckDb {
    db: RefCell<Sql>,
    program: Program,
    types: Vec<Vec<Ty>>,
    queries: Vec<Option<String>>,
    materialized: Vec<bool>,
    tick: u64,
    counters: Counters,
    work: Work,
    rounds: u64,
    marked: Option<u64>,
}
impl DuckDb {
    pub fn work(&self) -> Work {
        self.work
    }
    fn exec(&self, query: &str) -> Result<Vec<Value>> {
        self.db.borrow_mut().exec(query)
    }
    fn scalar(&self, query: &str) -> Result<i64> {
        self.exec(query)?
            .first()
            .and_then(|r| r["v"].as_i64())
            .ok_or_else(|| error(Stage::Settle, format!("expected integer v: {query}")))
    }
    fn rows(&self, table: &str, width: usize) -> Result<Vec<(Row, W)>> {
        self.exec(&format!("SELECT {} FROM {table}", sql::select(width, "")))?
            .into_iter()
            .map(|v| {
                let row = (0..width)
                    .map(|i| {
                        v[format!("c{i}")]
                            .as_i64()
                            .ok_or_else(|| error(Stage::Snapshot, format!("non-integer cell: {v}")))
                    })
                    .collect::<Result<Row>>()?;
                let w = v["w"]
                    .as_i64()
                    .ok_or_else(|| error(Stage::Snapshot, format!("non-integer weight: {v}")))?;
                Ok((row, w))
            })
            .collect()
    }
    fn replace(&self, table: &str, query: &str) -> Result<()> {
        self.exec(&format!("DELETE FROM {table}"))?;
        self.exec(&format!("INSERT INTO {table} {query}"))?;
        Ok(())
    }
    fn write_rows(&self, table: &str, rows: &[(Row, W)]) -> Result<()> {
        for chunk in rows.chunks(256) {
            let values = chunk
                .iter()
                .map(|(r, w)| {
                    format!(
                        "({})",
                        r.iter()
                            .chain([w])
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            self.exec(&format!("INSERT INTO {table} VALUES {values}"))?;
        }
        Ok(())
    }
    fn width(&self, rel: RelId) -> Result<usize> {
        self.program
            .rel(rel)
            .map(|r| r.cols.len())
            .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)))
    }
    fn evaluate(&mut self, id: NodeId, done: &mut BTreeSet<NodeId>) -> Result<()> {
        if done.contains(&id) {
            return Ok(());
        }
        // An explicit stack keeps generated plans with long projection chains off the Rust stack.
        let mut walk = vec![(id, false)];
        let mut active = BTreeSet::new();
        while let Some((node, expanded)) = walk.pop() {
            if done.contains(&node) {
                continue;
            }
            let op = self
                .program
                .nodes
                .get(node as usize)
                .ok_or_else(|| {
                    EngineError::new(Stage::Install, None, ErrorKind::UnknownNode(node))
                })?
                .clone();
            if !expanded {
                if !active.insert(node) {
                    return Err(error(Stage::Install, "node cycle"));
                }
                walk.push((node, true));
                let inputs = match &op {
                    Op::Get(_) => vec![],
                    Op::Join { inputs, .. } | Op::Union(inputs) => inputs.clone(),
                    Op::Antijoin { l, r, .. } => vec![*l, *r],
                    _ => op.type_inputs().to_vec(),
                };
                walk.extend(
                    inputs
                        .into_iter()
                        .rev()
                        .filter(|n| !done.contains(n))
                        .map(|n| (n, false)),
                );
                continue;
            }
            active.remove(&node);
            if self.materialized[node as usize] {
                let mode = if matches!(op, Op::Get(_) | Op::Negate(_) | Op::Threshold(_)) {
                    "incremental"
                } else {
                    "full"
                };
                self.exec(&format!("SET openivm_refresh_mode='{mode}'"))?;
                self.exec(&format!("PRAGMA refresh('n{node}')"))?;
                self.db.borrow_mut().work.refreshes += 1;
            } else if let Some(query) = self.queries[node as usize].clone() {
                self.replace(&format!("n{node}"), &query)?;
            } else {
                self.scalar_node(node, &op)?;
            }
            done.insert(node);
        }
        Ok(())
    }
    fn scalar_node(&mut self, node: NodeId, op: &Op) -> Result<()> {
        let input = *op
            .type_inputs()
            .first()
            .ok_or_else(|| error(Stage::Settle, "scalar node without input"))?;
        let input_rows = self.rows(&format!("n{input}"), self.types[input as usize].len())?;
        let mut output = BTreeMap::<Row, W>::new();
        for (mut row, w) in input_rows {
            match op {
                Op::Mint { functor, args, .. } => {
                    let args = args.iter().map(|c| row[*c as usize]).collect();
                    row.push(self.intern_term(*functor, &args)?);
                }
                Op::StrCons { mode, .. } => match mode {
                    StrMode::Construct { head, rest } => {
                        let Some(head) = self.text(row[*head as usize])? else {
                            continue;
                        };
                        let Some(rest) = self.text(row[*rest as usize])? else {
                            continue;
                        };
                        row.push(self.intern_text(&(head + &rest))?);
                    }
                    StrMode::Decompose { whole } => {
                        let Some(whole) = self.text(row[*whole as usize])? else {
                            continue;
                        };
                        let Some(head) = whole.chars().next() else {
                            continue;
                        };
                        row.push(self.intern_text(&head.to_string())?);
                        row.push(self.intern_text(&whole[head.len_utf8()..])?);
                    }
                },
                Op::Str { op, args, .. } => {
                    let texts = args
                        .iter()
                        .zip(op.args())
                        .map(|(c, k)| match k {
                            StrKind::Text => self.text(row[*c as usize]),
                            StrKind::Int => Ok(None),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let values = args
                        .iter()
                        .zip(op.args())
                        .zip(&texts)
                        .map(|((c, k), text)| match k {
                            StrKind::Int => Some(StrVal::Int(row[*c as usize])),
                            StrKind::Text => text.as_deref().map(StrVal::Text),
                        })
                        .collect::<Option<Vec<_>>>();
                    let Some(values) = values else {
                        continue;
                    };
                    match op.apply(&values) {
                        Some(StrOut::Holds) => {}
                        Some(StrOut::Int(n)) => row.push(n),
                        Some(StrOut::Text(t)) => row.push(self.intern_text(&t)?),
                        None => continue,
                    }
                }
                _ => return Err(error(Stage::Settle, "unhandled scalar node")),
            }
            *output.entry(row).or_default() += w;
        }
        self.exec(&format!("DELETE FROM n{node}"))?;
        self.write_rows(
            &format!("n{node}"),
            &output
                .into_iter()
                .filter(|(_, w)| *w != 0)
                .collect::<Vec<_>>(),
        )
    }
    fn recurse(&mut self, rec: &LetRec) -> Result<()> {
        for rel in &rec.ids {
            self.exec(&format!("DELETE FROM r{rel}"))?;
        }
        let mut rounds = 0;
        loop {
            for inner in &rec.nested {
                self.recurse(inner)?;
            }
            let mut done = BTreeSet::new();
            for (rel, body) in rec.ids.iter().zip(&rec.bodies) {
                self.evaluate(*body, &mut done)?;
                let width = self.width(*rel)?;
                let head = if width == 0 {
                    String::new()
                } else {
                    format!("{},", sql::cols(width, "").join(","))
                };
                self.replace(
                    &format!("next{rel}"),
                    &format!("SELECT {head}1::BIGINT AS w FROM n{body} WHERE w>0"),
                )?;
            }
            let mut changed = false;
            for rel in &rec.ids {
                changed |= self.scalar(&format!("SELECT count(*)::BIGINT AS v FROM ((SELECT * FROM next{rel} EXCEPT SELECT * FROM r{rel}) UNION ALL (SELECT * FROM r{rel} EXCEPT SELECT * FROM next{rel})) difference"))? > 0;
            }
            if !changed {
                break;
            }
            if rec.limit.is_some_and(|limit| rounds >= limit) {
                return Err(EngineError::new(
                    Stage::Settle,
                    rec.ids.first().copied(),
                    ErrorKind::LetRecLimit(
                        rec.limit
                            .ok_or_else(|| error(Stage::Settle, "missing recursion limit"))?,
                    ),
                ));
            }
            for rel in &rec.ids {
                self.replace(&format!("r{rel}"), &format!("SELECT * FROM next{rel}"))?;
            }
            rounds += 1;
            self.rounds += 1;
        }
        Ok(())
    }
    fn settle_inner(&mut self, frontier: Frontier) -> Result<Delta> {
        for change in frontier.changes {
            let rel = self
                .program
                .rel(change.rel)
                .filter(|r| r.kind == RelKind::Source)
                .ok_or_else(|| {
                    EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::UnknownRel(change.rel),
                    )
                })?;
            if change.row.len() != rel.cols.len() {
                return Err(EngineError::new(
                    Stage::Settle,
                    Some(rel.id),
                    ErrorKind::Arity {
                        expected: rel.cols.len(),
                        actual: change.row.len(),
                    },
                ));
            }
            if change.w != 1 && change.w != -1 {
                return Err(EngineError::new(
                    Stage::Settle,
                    Some(rel.id),
                    ErrorKind::Unsupported("weight other than +1/-1"),
                ));
            }
            let cond = if change.row.is_empty() {
                "true".into()
            } else {
                change
                    .row
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("c{i}={v}"))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            };
            let present = self.scalar(&format!(
                "SELECT count(*)::BIGINT AS v FROM r{} WHERE {cond}",
                rel.id
            ))? > 0;
            if present && change.w > 0 {
                return Err(EngineError::new(
                    Stage::Settle,
                    Some(rel.id),
                    ErrorKind::PresentInsert(change.row),
                ));
            }
            if change.w > 0 {
                self.write_rows(&format!("r{}", rel.id), &[(change.row, 1)])?;
            } else if present {
                self.exec(&format!("DELETE FROM r{} WHERE {cond}", rel.id))?;
            } else {
                tracing::warn!(relation = rel.id, "delete of absent row ignored");
                continue;
            }
            self.db.borrow_mut().work.rows_in += 1;
        }
        self.rounds = 0;
        let mut done = BTreeSet::new();
        for stratum in self.program.strata.clone() {
            match stratum {
                Stratum::Let { id, body } => {
                    self.evaluate(body, &mut done)?;
                    self.replace(&format!("r{id}"), &format!("SELECT * FROM n{body}"))?;
                }
                Stratum::LetRec(rec) => {
                    self.recurse(&rec)?;
                    done.clear();
                }
            }
        }
        let mut changes = Vec::new();
        for rel in &self.program.outputs {
            let width = self.width(*rel)?;
            let head = if width == 0 {
                String::new()
            } else {
                format!("{},", sql::cols(width, "").join(","))
            };
            let q = sql::consolidate(
                &format!("SELECT * FROM r{rel} UNION ALL SELECT {head}-w AS w FROM out{rel}"),
                width,
            );
            for (row, w) in self.rows(&format!("({q}) result"), width)? {
                changes.push((*rel, row, w));
            }
            self.replace(&format!("out{rel}"), &format!("SELECT * FROM r{rel}"))?;
        }
        changes.sort();
        Ok(Delta {
            tick: self.tick,
            changes,
        })
    }
    fn intern_term(&mut self, functor: RelId, args: &Row) -> Result<Cell> {
        let rel = self
            .program
            .rel(functor)
            .filter(|r| r.kind == RelKind::Constructor)
            .ok_or_else(|| {
                EngineError::new(Stage::Settle, Some(functor), ErrorKind::UnknownRel(functor))
            })?
            .clone();
        if rel.cols.len() != args.len() + 1 {
            return Err(EngineError::new(
                Stage::Settle,
                Some(functor),
                ErrorKind::Arity {
                    expected: rel.cols.len().saturating_sub(1),
                    actual: args.len(),
                },
            ));
        }
        let payload = serde_json::to_string(args).map_err(|e| error(Stage::Settle, e))?;
        let key = format!("term:{functor}:{payload}");
        if let Some(id) = self.lookup_key(&key)? {
            return Ok(id);
        }
        let id = self.next_id()?;
        let term = Term {
            functor: Arc::from(rel.name),
            args: args.clone(),
            types: rel.cols[1..].to_vec(),
            text: None,
        };
        let keybytes = self.term_key(id, &term)?;
        self.exec(&format!(
            "INSERT INTO dict VALUES ({id},{},'term',{}, {functor},{})",
            literal(&key),
            literal(&payload),
            literal(&keybytes)
        ))?;
        let mut row = vec![id];
        row.extend(args);
        self.write_rows(&format!("r{functor}"), &[(row, 1)])?;
        Ok(id)
    }
    fn term_key(&self, id: Cell, term: &Term) -> Result<String> {
        let mut failure = None;
        let bytes = sort_key(id, &mut |child| {
            if child == id {
                return Some(term.clone());
            }
            match self.term(child) {
                Ok(t) => t,
                Err(e) => {
                    failure = Some(e);
                    None
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        Ok(bytes.iter().map(|b| format!("{b:02X}")).collect())
    }
    fn term(&self, id: Cell) -> Result<Option<Term>> {
        let rows = self.exec(&format!(
            "SELECT kind,payload,functor FROM dict WHERE id={id}"
        ))?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        match row["kind"].as_str() {
            Some("text") => Ok(Some(Term {
                functor: Arc::from(""),
                args: vec![],
                types: vec![],
                text: Some(Arc::from(
                    row["payload"]
                        .as_str()
                        .ok_or_else(|| error(Stage::Snapshot, "text payload"))?,
                )),
            })),
            Some("term") => {
                let fid = row["functor"]
                    .as_u64()
                    .ok_or_else(|| error(Stage::Snapshot, "term functor"))?
                    as RelId;
                let rel = self
                    .program
                    .rel(fid)
                    .ok_or_else(|| error(Stage::Snapshot, "unknown term functor"))?;
                let args = serde_json::from_str(
                    row["payload"]
                        .as_str()
                        .ok_or_else(|| error(Stage::Snapshot, "term payload"))?,
                )
                .map_err(|e| error(Stage::Snapshot, e))?;
                Ok(Some(Term {
                    functor: Arc::from(rel.name.as_str()),
                    args,
                    types: rel.cols[1..].to_vec(),
                    text: None,
                }))
            }
            Some("any") => Ok(None),
            _ => Err(error(Stage::Snapshot, "unknown dictionary kind")),
        }
    }
    fn lookup_key(&self, key: &str) -> Result<Option<Cell>> {
        self.exec(&format!("SELECT id FROM dict WHERE key={}", literal(key)))?
            .first()
            .map(|r| {
                r["id"]
                    .as_i64()
                    .ok_or_else(|| error(Stage::Snapshot, "dictionary id"))
            })
            .transpose()
    }
    fn next_id(&self) -> Result<Cell> {
        self.scalar("SELECT (coalesce(max(id),0)+1)::BIGINT AS v FROM dict")
    }
}
impl Engine for DuckDb {
    fn install(program: &Program) -> Result<Self> {
        let mut memo = Vec::new();
        let types = (0..program.nodes.len())
            .map(|n| {
                program
                    .node_types_memo(n as NodeId, &mut memo)
                    .ok_or_else(|| {
                        EngineError::new(Stage::Install, None, ErrorKind::UnknownNode(n as NodeId))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        for op in &program.nodes {
            if matches!(op, Op::Delay(_)) {
                return Err(EngineError::new(
                    Stage::Install,
                    None,
                    ErrorKind::Unsupported("Delay"),
                ));
            }
        }
        let mut engine = Self {
            db: RefCell::new(Sql::open()?),
            program: program.clone(),
            types,
            queries: vec![],
            materialized: vec![false; program.nodes.len()],
            tick: 0,
            counters: Counters::default(),
            work: Work::default(),
            rounds: 0,
            marked: None,
        };
        engine.exec("CREATE TABLE dict(id BIGINT PRIMARY KEY,key VARCHAR UNIQUE,kind VARCHAR,payload VARCHAR,functor BIGINT,sortkey VARCHAR)")?;
        engine.exec("CREATE TABLE texts(id BIGINT PRIMARY KEY,value VARCHAR UNIQUE)")?;
        for rel in &program.rels {
            engine.exec(&format!(
                "CREATE TABLE r{}({})",
                rel.id,
                sql::decl(rel.cols.len())
            ))?;
        }
        for output in &program.outputs {
            engine.exec(&format!(
                "CREATE TABLE out{output}({})",
                sql::decl(engine.width(*output)?)
            ))?;
        }
        for (n, t) in engine.types.iter().enumerate() {
            engine.exec(&format!("CREATE TABLE n{n}({})", sql::decl(t.len())))?;
        }
        let mut text_ids = Vec::new();
        for text in &program.texts {
            text_ids.push(engine.intern_text(text)?);
        }
        let nil = if program.uses_strings() {
            engine.intern_text("")?
        } else {
            0
        };
        let recursive = program
            .strata
            .iter()
            .any(|s| matches!(s, Stratum::LetRec(_)));
        for s in &program.strata {
            if let Stratum::LetRec(rec) = s {
                if rec.nested.iter().any(|scope| !scope.nested.is_empty()) {
                    return Err(EngineError::new(
                        Stage::Install,
                        rec.ids.first().copied(),
                        ErrorKind::Unsupported("LetRec nested two deep"),
                    ));
                }
                for scope in std::iter::once(rec).chain(&rec.nested) {
                    if scope.ids.len() != scope.bodies.len()
                        || scope.ids.is_empty()
                        || scope.limit == Some(0)
                    {
                        return Err(EngineError::new(
                            Stage::Install,
                            scope.ids.first().copied(),
                            ErrorKind::Unsupported("invalid LetRec scope"),
                        ));
                    }
                    for id in &scope.ids {
                        engine.exec(&format!(
                            "CREATE TABLE next{id}({})",
                            sql::decl(engine.width(*id)?)
                        ))?;
                    }
                }
                eprintln!("ivm-duckdb: LetRec {:?}: SQL full recompute from empty; OpenIVM has no recursive IVM",rec.ids);
            }
        }
        for n in 0..program.nodes.len() {
            let query = sql::query(program, n as NodeId, &engine.types, &text_ids, nil)?;
            let dictionary_read = query
                .as_ref()
                .is_some_and(|q| q.contains(" FROM dict WHERE") || q.contains(" FROM texts WHERE"));
            let materialize = !recursive && query.is_some() && !dictionary_read;
            if materialize {
                engine.exec(&format!("DROP TABLE n{n}"))?;
                let query_text = query
                    .as_ref()
                    .ok_or_else(|| error(Stage::Install, "missing node SQL"))?;
                match engine.exec(&format!("CREATE MATERIALIZED VIEW n{n} AS {query_text}")) {
                    Ok(_) => {
                        engine.materialized[n] = true;
                        if !matches!(
                            program.nodes[n],
                            Op::Get(_) | Op::Negate(_) | Op::Threshold(_)
                        ) {
                            eprintln!("ivm-duckdb: node {n}: full refresh for signed-weight consolidation; pinned incremental grouping lost retractions");
                        }
                        let strategy = engine.scalar(&format!(
                            "SELECT type::BIGINT AS v FROM openivm_views WHERE view_name='n{n}'"
                        ))?;
                        if matches!(strategy, 3 | 5 | 6 | 9) {
                            eprintln!("ivm-duckdb: node {n}: OpenIVM refresh strategy {strategy} includes recompute");
                        }
                    }
                    Err(e) => {
                        // A failed extension compilation is an install error. Continuing could leave partial extension catalog state.
                        return Err(e);
                    }
                }
            } else {
                eprintln!(
                    "ivm-duckdb: node {n} {:?}: recompute ({})",
                    program.nodes[n],
                    if recursive {
                        "recursive program"
                    } else if dictionary_read {
                        "dictionary lookup excluded from OpenIVM tracking"
                    } else {
                        "dictionary/string scalar boundary"
                    }
                );
            }
            engine.queries.push(query);
        }
        engine.db.borrow_mut().stage = Stage::Settle;
        Ok(engine)
    }
    fn settle(&mut self, frontier: Frontier) -> Result<Delta> {
        self.db.borrow_mut().work = Work::default();
        let before = self.scalar("SELECT count(*)::BIGINT AS v FROM dict")?;
        self.db.borrow_mut().checkpoint("settle.backup")?;
        let settled = self.settle_inner(frontier).and_then(|delta| {
            let interned = self.scalar("SELECT count(*)::BIGINT AS v FROM dict")? - before;
            Ok((delta, interned))
        });
        match settled {
            Ok((delta, interned)) => {
                self.tick += 1;
                self.work = self.db.borrow().work;
                self.counters = Counters {
                    rows_written: delta.changes.len() as u64,
                    statements: Some(self.work.statements),
                    rounds: Some(self.rounds),
                    interned: Some(interned as u64),
                    ..Counters::default()
                };
                tracing::info!(
                    statements = self.work.statements,
                    rows_in = self.work.rows_in,
                    rows_out = self.work.rows_out,
                    refreshes = self.work.refreshes,
                    "ivm-duckdb settle"
                );
                Ok(delta)
            }
            Err(e) => {
                self.db
                    .borrow_mut()
                    .restore("settle.backup")
                    .map_err(|restore| {
                        error(
                            Stage::Settle,
                            format!("settle failed: {e}; restore failed: {restore}"),
                        )
                    })?;
                self.work = self.db.borrow().work;
                Err(e)
            }
        }
    }
    fn counters(&self) -> Counters {
        self.counters
    }
    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>> {
        let width = self.width(rel)?;
        let mut rows = self.rows(&format!("r{rel}"), width)?;
        rows.sort();
        Ok(rows)
    }
    fn intern_snapshot(&self, functor: RelId) -> Result<Vec<(Row, W)>> {
        if !self
            .program
            .rel(functor)
            .is_some_and(|r| r.kind == RelKind::Constructor)
        {
            return Err(EngineError::new(
                Stage::Snapshot,
                Some(functor),
                ErrorKind::UnknownRel(functor),
            ));
        }
        self.snapshot(functor)
    }
    fn intern_text(&mut self, text: &str) -> Result<Cell> {
        let key = format!("text:{text}");
        if let Some(id) = self.lookup_key(&key)? {
            return Ok(id);
        }
        let id = self.next_id()?;
        let term = Term {
            functor: Arc::from(""),
            args: vec![],
            types: vec![],
            text: Some(Arc::from(text)),
        };
        let sortkey = self.term_key(id, &term)?;
        self.exec(&format!(
            "INSERT INTO dict VALUES ({id},{},'text',{},NULL,{})",
            literal(&key),
            literal(text),
            literal(&sortkey)
        ))?;
        self.exec(&format!(
            "INSERT INTO texts VALUES ({id},{})",
            literal(text)
        ))?;
        Ok(id)
    }
    fn intern_terms(&mut self, terms: &[(RelId, Row)]) -> Result<Vec<Cell>> {
        terms
            .iter()
            .map(|(f, args)| self.intern_term(*f, args))
            .collect()
    }
    fn text(&self, id: Cell) -> Result<Option<String>> {
        self.exec(&format!("SELECT value FROM texts WHERE id={id}"))?
            .first()
            .map(|r| {
                r["value"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| error(Stage::Snapshot, "text value"))
            })
            .transpose()
    }
    fn intern_any(&mut self, value: &AnyValue) -> Result<Cell> {
        let value = match value {
            AnyValue::Real(bits) if f64::from_bits(*bits).is_nan() => &AnyValue::Null,
            _ => value,
        };
        let payload = serde_json::to_string(value).map_err(|e| error(Stage::Settle, e))?;
        let key = format!("any:{payload}");
        if let Some(id) = self.lookup_key(&key)? {
            return Ok(id);
        }
        let id = self.next_id()?;
        self.exec(&format!(
            "INSERT INTO dict VALUES ({id},{},'any',{},NULL,NULL)",
            literal(&key),
            literal(&payload)
        ))?;
        Ok(id)
    }
    fn any_value(&self, id: Cell) -> Result<AnyValue> {
        let rows = self.exec(&format!(
            "SELECT payload FROM dict WHERE id={id} AND kind='any'"
        ))?;
        let payload = rows
            .first()
            .and_then(|r| r["payload"].as_str())
            .ok_or_else(|| error(Stage::Snapshot, "unknown Any cell"))?;
        serde_json::from_str(payload).map_err(|e| error(Stage::Snapshot, e))
    }
    fn rewinds(&self) -> bool {
        true
    }
    fn mark(&mut self) -> Result<()> {
        self.db.borrow_mut().checkpoint("mark.backup")?;
        self.marked = Some(self.tick);
        Ok(())
    }
    fn rewind(&mut self) -> Result<()> {
        let tick = self
            .marked
            .ok_or_else(|| error(Stage::Settle, "rewind requires mark"))?;
        self.db.borrow_mut().restore("mark.backup")?;
        self.tick = tick;
        self.counters = Counters::default();
        self.work = Work::default();
        Ok(())
    }
}
