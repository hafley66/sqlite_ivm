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
    bags: BTreeSet<String>,
    view_reports: Vec<Value>,
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
    pub fn view_reports(&self) -> &[Value] {
        &self.view_reports
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
        let query = if self.bags.contains(table) {
            sql::counted(table, width)
        } else {
            format!("SELECT {} FROM {table}", sql::select(width, ""))
        };
        self.exec(&query)?
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
            if self.bags.contains(table) {
                for (row, w) in chunk {
                    if *w < 0 {
                        return Err(error(Stage::Settle, "negative persistent bag multiplicity"));
                    }
                    let values = if row.is_empty() {
                        "1".into()
                    } else {
                        row.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    };
                    self.exec(&format!(
                        "INSERT INTO {table} SELECT {values} FROM range({w})"
                    ))?;
                }
                continue;
            }
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
    fn create_view(&mut self, name: &str, operator: &str, query: &str) -> Result<()> {
        self.exec(&format!("CREATE MATERIALIZED VIEW {name} AS {query}"))?;
        let warning = self.db.borrow().diagnostics.clone();
        let class = self.scalar(&format!(
            "SELECT type::BIGINT AS v FROM openivm_views WHERE view_name={}",
            literal(name)
        ))?;
        let class_name = match class {
            0 => "AGGREGATE_GROUP",
            1 => "SIMPLE_AGGREGATE",
            2 => "SIMPLE_PROJECTION",
            3 => "FULL_REFRESH",
            4 => "AGGREGATE_HAVING",
            5 => "WINDOW_PARTITION",
            6 => "GROUP_RECOMPUTE",
            7 => "TOP_K",
            8 => "DISTINCT_INCREMENTAL",
            9 => "SEMI_ANTI_RECOMPUTE",
            _ => {
                return Err(error(
                    Stage::Install,
                    format!("unknown OpenIVM refresh type {class}"),
                ))
            }
        };
        let details = self.exec(&format!("SELECT detail FROM openivm_refresh_profile WHERE view_name={} AND step_name='create_compile_classification' ORDER BY profile_timestamp DESC LIMIT 1",literal(name)))?;
        let detail = details
            .first()
            .and_then(|v| v["detail"].as_str())
            .ok_or_else(|| error(Stage::Install, "missing OpenIVM classification profile"))?;
        let report = serde_json::json!({"view":name,"operator":operator,"type":class_name,"type_code":class,"openivm_detail":detail,"openivm_warning":warning,"sql":query});
        eprintln!("VIEW {report}");
        self.view_reports.push(report);
        if class == 3 && warning.trim().is_empty() {
            return Err(error(Stage::Install,format!("OpenIVM assigned FULL_REFRESH to {name} without a creation explanation; SQL: {query}")));
        }
        Ok(())
    }
    fn install_nodes(
        &mut self,
        root: NodeId,
        installed: &mut BTreeSet<NodeId>,
        recursive: &BTreeSet<NodeId>,
        texts: &[Cell],
        nil: Cell,
    ) -> Result<()> {
        let mut walk = vec![(root, false)];
        let mut active = BTreeSet::new();
        while let Some((node, expanded)) = walk.pop() {
            if installed.contains(&node) {
                continue;
            }
            let op = self
                .program
                .nodes
                .get(node as usize)
                .ok_or_else(|| error(Stage::Install, "unknown node"))?
                .clone();
            let inputs = sql::inputs(&op);
            if !expanded {
                if !active.insert(node) {
                    return Err(error(Stage::Install, "node cycle"));
                }
                walk.push((node, true));
                walk.extend(inputs.into_iter().rev().map(|n| (n, false)));
                continue;
            }
            active.remove(&node);
            let weighted_query = sql::query(&self.program, node, &self.types, texts, nil, false)?;
            let dictionary = weighted_query
                .as_ref()
                .is_some_and(|q| q.contains(" FROM dict WHERE") || q.contains(" FROM texts WHERE"));
            let inputs_are_bags = match op {
                Op::Get(r) => self.bags.contains(&format!("r{r}")),
                _ => inputs.iter().all(|n| self.bags.contains(&format!("n{n}"))),
            };
            let reason = if recursive.contains(&node) {
                Some("LetRec round body")
            } else if matches!(op, Op::Negate(_)) {
                Some("IR Negate has persistent signed weights; SQL bags have nonnegative counts")
            } else if dictionary {
                Some(
                    "dictionary scalar lookup; excluded from OpenIVM NUL-sensitive change tracking",
                )
            } else if !inputs_are_bags {
                Some("weighted input from an explicit IR recompute boundary")
            } else {
                None
            };
            let bag = reason.is_none();
            let query = if bag {
                sql::query(&self.program, node, &self.types, texts, nil, true)?
            } else {
                weighted_query
            };
            if bag {
                self.exec(&format!("DROP TABLE n{node}"))?;
                if let Some(ref q) = query {
                    self.create_view(&format!("n{node}"), sql::operator(&op), q)?;
                    self.materialized[node as usize] = true;
                } else {
                    self.exec(&format!(
                        "CREATE TABLE n{node}({})",
                        sql::bag_decl(self.types[node as usize].len())
                    ))?;
                    eprintln!("BOUNDARY node={node} operator={} reason=Rust dictionary/string scalar evaluation; bag output tracked by OpenIVM",sql::operator(&op));
                }
                self.bags.insert(format!("n{node}"));
            } else {
                eprintln!(
                    "BOUNDARY node={node} operator={} reason={}",
                    sql::operator(&op),
                    reason
                        .ok_or_else(|| error(Stage::Install, "missing weighted boundary reason"))?
                );
            }
            let canonical = if bag {
                sql::counted(&format!("n{node}"), self.types[node as usize].len())
            } else {
                format!("SELECT * FROM n{node}")
            };
            self.exec(&format!("CREATE VIEW zn{node} AS {canonical}"))?;
            self.queries[node as usize] = query;
            installed.insert(node);
        }
        Ok(())
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
                let inputs = sql::inputs(&op);
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
        let since = self
            .exec("SELECT current_timestamp::VARCHAR AS stamp")?
            .first()
            .and_then(|v| v["stamp"].as_str())
            .ok_or_else(|| error(Stage::Settle, "missing delta timestamp"))?
            .to_string();
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
        // DD feeds newly interned constructor rows until every reader has consumed them.
        // A Join-only shortcut misses constructor projections and already-evaluated readers.
        let constructor_reads = self
            .program
            .nodes
            .iter()
            .filter_map(|op| match op {
                Op::Get(r)
                    if self
                        .program
                        .rel(*r)
                        .is_some_and(|rel| rel.kind == RelKind::Constructor) =>
                {
                    Some(*r)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let counts = constructor_reads
            .iter()
            .map(|r| format!("(SELECT count(*) FROM r{r})"))
            .collect::<Vec<_>>();
        let count_query = if counts.is_empty() {
            None
        } else {
            Some(format!("SELECT ({})::BIGINT AS v", counts.join("+")))
        };
        loop {
            let before = count_query.as_ref().map(|q| self.scalar(q)).transpose()?;
            let mut done = BTreeSet::new();
            for stratum in self.program.strata.clone() {
                match stratum {
                    Stratum::Let { body, .. } => {
                        self.evaluate(body, &mut done)?;
                    }
                    Stratum::LetRec(rec) => {
                        self.recurse(&rec)?;
                        done.clear();
                    }
                }
            }
            let after = count_query.as_ref().map(|q| self.scalar(q)).transpose()?;
            if before == after {
                break;
            }
        }
        let mut changes = Vec::new();
        for rel in &self.program.outputs {
            let width = self.width(*rel)?;
            if self.bags.contains(&format!("r{rel}")) {
                self.exec(&format!("PRAGMA refresh('out{rel}')"))?;
                self.db.borrow_mut().work.refreshes += 1;
                let head = if width == 0 {
                    String::new()
                } else {
                    format!("{},", sql::cols(width, "").join(","))
                };
                let query = format!("SELECT {head}openivm_multiplicity::BIGINT AS w FROM openivm_delta_out{rel} WHERE openivm_timestamp>={}::TIMESTAMP",literal(&since));
                let mut consolidated = BTreeMap::<Row, W>::new();
                for (row, w) in self.rows(&format!("({query}) delta"), width)? {
                    *consolidated.entry(row).or_default() += w;
                }
                changes.extend(
                    consolidated
                        .into_iter()
                        .filter(|(_, w)| *w != 0)
                        .map(|(row, w)| (*rel, row, w)),
                );
                // A real downstream snapshot MV retains the sink's delta until it is read.
                // Its refresh then advances OpenIVM's consumer cursor and permits cleanup.
                self.exec(&format!("PRAGMA refresh('read{rel}')"))?;
                self.db.borrow_mut().work.refreshes += 1;
                continue;
            }
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
            queries: vec![None; program.nodes.len()],
            materialized: vec![false; program.nodes.len()],
            bags: BTreeSet::new(),
            view_reports: Vec::new(),
            tick: 0,
            counters: Counters::default(),
            work: Work::default(),
            rounds: 0,
            marked: None,
        };
        engine.exec("CREATE TABLE dict(id BIGINT PRIMARY KEY,key VARCHAR UNIQUE,kind VARCHAR,payload VARCHAR,functor BIGINT,sortkey VARCHAR)")?;
        engine.exec("CREATE TABLE texts(id BIGINT PRIMARY KEY,value VARCHAR UNIQUE)")?;
        for rel in &program.rels {
            let table = format!("r{}", rel.id);
            let bag = matches!(rel.kind, RelKind::Source | RelKind::Constructor);
            engine.exec(&format!(
                "CREATE TABLE {table}({})",
                if bag {
                    sql::bag_decl(rel.cols.len())
                } else {
                    sql::decl(rel.cols.len())
                }
            ))?;
            if bag {
                engine.bags.insert(table.clone());
            }
            let q = if bag {
                sql::counted(&table, rel.cols.len())
            } else {
                format!("SELECT * FROM {table}")
            };
            engine.exec(&format!("CREATE VIEW z{table} AS {q}"))?;
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
        let mut recursive_nodes = BTreeSet::new();
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
                let mut walk = rec.bodies.clone();
                for nested in &rec.nested {
                    walk.extend(&nested.bodies);
                }
                while let Some(n) = walk.pop() {
                    if recursive_nodes.insert(n) {
                        walk.extend(sql::inputs(&program.nodes[n as usize]));
                    }
                }
                eprintln!("ivm-duckdb: LetRec {:?}: SQL full recompute from empty; OpenIVM has no recursive IVM",rec.ids);
            }
        }
        let mut installed = BTreeSet::new();
        for stratum in &program.strata {
            match stratum {
                Stratum::Let { id, body } => {
                    engine.install_nodes(
                        *body,
                        &mut installed,
                        &recursive_nodes,
                        &text_ids,
                        nil,
                    )?;
                    engine.exec(&format!("DROP VIEW zr{id}"))?;
                    engine.exec(&format!("DROP TABLE r{id}"))?;
                    engine.exec(&format!("CREATE VIEW r{id} AS SELECT * FROM n{body}"))?;
                    if engine.bags.contains(&format!("n{body}")) {
                        engine.bags.insert(format!("r{id}"));
                    }
                    engine.exec(&format!("CREATE VIEW zr{id} AS SELECT * FROM zn{body}"))?;
                }
                Stratum::LetRec(rec) => {
                    for scope in std::iter::once(rec).chain(&rec.nested) {
                        for body in &scope.bodies {
                            engine.install_nodes(
                                *body,
                                &mut installed,
                                &recursive_nodes,
                                &text_ids,
                                nil,
                            )?;
                        }
                    }
                }
            }
        }
        for n in 0..program.nodes.len() {
            engine.install_nodes(
                n as NodeId,
                &mut installed,
                &recursive_nodes,
                &text_ids,
                nil,
            )?;
        }
        for rel in &program.outputs {
            if engine.bags.contains(&format!("r{rel}")) {
                engine.exec(&format!("DROP TABLE out{rel}"))?;
                engine.create_view(
                    &format!("out{rel}"),
                    "Output",
                    &format!(
                        "SELECT {} FROM r{rel}",
                        sql::bag_select(engine.width(*rel)?, "")
                    ),
                )?;
                engine.create_view(
                    &format!("read{rel}"),
                    "OutputSnapshot",
                    &format!(
                        "SELECT {} FROM out{rel}",
                        sql::bag_select(engine.width(*rel)?, "")
                    ),
                )?;
                engine.bags.insert(format!("read{rel}"));
            }
        }
        engine.exec("SET openivm_profile_refresh=false")?;
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
        let table = if self.bags.contains(&format!("read{rel}")) {
            format!("read{rel}")
        } else {
            format!("r{rel}")
        };
        let mut rows = self.rows(&table, width)?;
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
        let mut rows = self.rows(&format!("r{functor}"), self.width(functor)?)?;
        rows.sort();
        Ok(rows)
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
