//! Typed Engine adapter. The engine opens and owns its SQLite connection.

use crate::{
    catalog, Cell, Program as SqlProgram, Sign,
};
use ivm_engine::{Counters, Engine, EngineError, ErrorKind, Stage};
use ivm_ir::{Delta, Expr, Frontier, NodeId, Op, Program, RelId, RelKind, Row, Stratum, W};
use sqlite_ext::rusqlite::{self, Connection};
use std::collections::{BTreeSet, HashSet};

pub struct Sqlite {
    /// Work counting on `db` (`Engine::install` with `ivm_sqlite::work` enabled, or
    /// `install_counted`); declared first so it unregisters before `db` closes.
    work: Option<crate::work::WorkLog>,
    /// The database every installed program, term and source table lives in.
    pub db: Connection,
    programs: Vec<OutputProgram>,
    ir: Program,
    tick: u64,
    counters: Counters,
    /// `mark` opened the `ivm_engine_mark` savepoint: every later write is journaled under it
    /// and `rewind` rolls back to it.
    marked: bool,
}

impl Sqlite {
    /// SQL statements the installed programs may issue while settling a frontier.
    pub fn statements(&self) -> Vec<&str> {
        let mut all = Vec::new();
        for output in &self.programs {
            let crate::plan::Root::Nodes(nodes) = &output.program.inner.plan.root else { unreachable!() };
            all.extend(nodes.statements());
        }
        all.into_iter().filter(|sql| !sql.is_empty()).collect()
    }
}

struct OutputProgram {
    rel: RelId,
    program: SqlProgram,
    threshold: bool,
    members: Vec<(RelId, usize)>,
}

fn error(stage: Stage, e: impl std::fmt::Display) -> EngineError {
    EngineError::new(stage, None, ErrorKind::Worker(e.to_string()))
}

fn plan_error(e: crate::EngineError) -> EngineError {
    match e.kind {
        crate::ErrorKind::Unsupported(why) => EngineError::new(Stage::Install, None, ErrorKind::Unsupported(why)),
        _ => error(Stage::Install, e),
    }
}

fn row_of(row: &rusqlite::Row<'_>, width: usize) -> rusqlite::Result<Row> {
    (0..width).map(|i| row.get(i)).collect()
}

fn text_refs(expr: &Expr, used: &mut BTreeSet<u32>) {
    let mut walk = vec![expr];
    while let Some(next) = walk.pop() {
        match next {
            Expr::Text(id) => { used.insert(*id); }
            Expr::Call(_, args) => walk.extend(args.iter()),
            _ => {}
        }
    }
}

fn remap_text(expr: &mut Expr, ids: &[u32]) {
    let mut walk = vec![expr];
    while let Some(next) = walk.pop() {
        match next {
            Expr::Text(id) => {
                if let Ok(mapped) = ids.binary_search(id) { *id = mapped as u32; }
            }
            Expr::Call(_, args) => walk.extend(args.iter_mut()),
            _ => {}
        }
    }
}

/// Depth-first in input order with an explicit stack; `relations` keeps the visit order.
fn visit_node(ir: &Program, node: NodeId, seen: &mut [bool], relations: &mut Vec<RelId>) {
    let mut walk = vec![node];
    while let Some(node) = walk.pop() {
        let Some(slot) = seen.get_mut(node as usize) else { continue };
        if *slot { continue; }
        *slot = true;
        match &ir.nodes[node as usize] {
            Op::Get(id) => relations.push(*id),
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Negate(input) | Op::Reduce { input, .. } | Op::Threshold(input)
            | Op::TopK { input, .. } | Op::Window { input, .. } | Op::Delay(input) => walk.push(*input),
            Op::Union(inputs) | Op::Join { inputs, .. } => walk.extend(inputs.iter().rev()),
            Op::Antijoin { l, r, .. } => walk.extend([*r, *l]),
        }
    }
}

/// Each catalog program owns one output. Retain the strata that define that
/// output and the relations its bodies read, in their original order.
fn output_program(ir: &Program, output: RelId) -> Program {
    let mut needed = vec![false; ir.strata.len()];
    let mut pending = vec![output];
    while let Some(id) = pending.pop() {
        let Some((index, stratum)) = ir.strata.iter().enumerate().find(|(_, stratum)| match stratum {
            Stratum::Let { id: defined, .. } => *defined == id,
            Stratum::LetRec(rec) => rec.ids.contains(&id),
        }) else { continue };
        if needed[index] { continue; }
        needed[index] = true;
        let mut seen = vec![false; ir.nodes.len()];
        let mut relations = Vec::new();
        match stratum {
            Stratum::Let { body, .. } => visit_node(ir, *body, &mut seen, &mut relations),
            Stratum::LetRec(rec) => {
                for &body in rec.bodies.iter().chain(rec.nested.iter().flat_map(|inner| &inner.bodies)) {
                    visit_node(ir, body, &mut seen, &mut relations);
                }
            }
        }
        pending.extend(relations);
    }
    let mut single = ir.clone();
    single.outputs = vec![output];
    single.strata = ir.strata.iter().zip(needed).filter_map(|(stratum, keep)| keep.then(|| stratum.clone())).collect();
    let mut used = vec![false; ir.nodes.len()];
    for stratum in &single.strata {
        let mut relations = Vec::new();
        match stratum {
            Stratum::Let { body, .. } => visit_node(ir, *body, &mut used, &mut relations),
            Stratum::LetRec(rec) => {
                for &body in rec.bodies.iter().chain(rec.nested.iter().flat_map(|inner| &inner.bodies)) {
                    visit_node(ir, body, &mut used, &mut relations);
                }
            }
        }
    }
    let mut remap = vec![0; ir.nodes.len()];
    let mut next = 0;
    for (old, keep) in used.iter().enumerate() {
        if *keep { remap[old] = next; next += 1; }
    }
    single.nodes = ir.nodes.iter().enumerate().filter_map(|(old, op)| used[old].then(|| {
        let mut op = op.clone();
        let mut rewrite = |id: &mut NodeId| {
            if let Some(mapped) = remap.get(*id as usize) { *id = *mapped; }
        };
        match &mut op {
            Op::Get(_) => {}
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Negate(input) | Op::Reduce { input, .. } | Op::Threshold(input)
            | Op::TopK { input, .. } | Op::Window { input, .. } | Op::Delay(input) => rewrite(input),
            Op::Union(inputs) | Op::Join { inputs, .. } => inputs.iter_mut().for_each(&mut rewrite),
            Op::Antijoin { l, r, .. } => { rewrite(l); rewrite(r); }
        }
        op
    })).collect();
    let mut relations = HashSet::from([output]);
    for stratum in &single.strata {
        match stratum {
            Stratum::Let { id, .. } => { relations.insert(*id); }
            Stratum::LetRec(rec) => {
                relations.extend(rec.ids.iter().copied());
                relations.extend(rec.nested.iter().flat_map(|inner| inner.ids.iter().copied()));
            }
        }
    }
    for op in &single.nodes {
        match op {
            Op::Get(id) => { relations.insert(*id); }
            Op::Mint { functor, .. } => { relations.insert(*functor); }
            _ => {}
        }
    }
    single.rels.retain(|relation| relations.contains(&relation.id));
    let mut used_texts = BTreeSet::new();
    for op in &single.nodes {
        if let Op::Mfp { filter, map, .. } = op {
            filter.iter().chain(map).for_each(|expr| text_refs(expr, &mut used_texts));
        }
    }
    let text_ids = used_texts.into_iter().collect::<Vec<_>>();
    single.texts = text_ids.iter().filter_map(|id| ir.texts.get(*id as usize).cloned()).collect();
    for op in &mut single.nodes {
        if let Op::Mfp { filter, map, .. } = op {
            filter.iter_mut().chain(map).for_each(|expr| remap_text(expr, &text_ids));
        }
    }
    for stratum in &mut single.strata {
        match stratum {
            Stratum::Let { body, .. } => {
                if let Some(mapped) = remap.get(*body as usize) { *body = *mapped; }
            }
            Stratum::LetRec(rec) => {
                let nested = rec.nested.iter_mut().flat_map(|inner| inner.bodies.iter_mut());
                rec.bodies.iter_mut().chain(nested).for_each(|body| {
                    if let Some(mapped) = remap.get(*body as usize) { *body = *mapped; }
                });
            }
        }
    }
    single
}

/// One physical plan carries every typed output. The first column identifies
/// the output relation; remaining columns hold its row, padded to a common width.
fn bundled_program(ir: &Program) -> Result<(Program, Vec<(RelId, usize)>), EngineError> {
    let mut bundle = ir.clone();
    let members = ir.outputs.iter().map(|id| {
        ir.rel(*id).map(|rel| (*id, rel.cols.len())).ok_or_else(||
            EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id)))
    }).collect::<Result<Vec<_>, _>>()?;
    let width = members.iter().map(|(_, width)| *width).max().unwrap_or(0);
    let mut branches = Vec::with_capacity(members.len());
    for &(rel, arity) in &members {
        let get = bundle.nodes.len() as NodeId;
        bundle.nodes.push(Op::Get(rel));
        let branch = bundle.nodes.len() as NodeId;
        let mut map = vec![Expr::Lit(rel as i64)];
        map.extend((arity..width).map(|_| Expr::Lit(0)));
        let mut project = vec![arity as u16];
        project.extend((0..arity).map(|column| column as u16));
        project.extend((arity + 1..=width).map(|column| column as u16));
        bundle.nodes.push(Op::Mfp { input: get, filter: vec![], map, project });
        branches.push(branch);
    }
    let body = bundle.nodes.len() as NodeId;
    bundle.nodes.push(Op::Union(branches));
    let id = bundle.rels.iter().map(|rel| rel.id).max().unwrap_or(0).checked_add(1)
        .ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("relation id space exhausted")))?;
    let name = format!("__ivm_bundle_{}", ir.rel(ir.outputs[0]).unwrap().name);
    bundle.rels.push(ivm_ir::Relation { id, name, cols: vec![ivm_ir::Ty::Int; width + 1], kind: RelKind::Derived });
    bundle.strata.push(Stratum::Let { id, body });
    bundle.outputs = vec![id];
    Ok((bundle, members))
}

/// Most attached in-memory schemas `Engine::install` fills with node tables, `SHARD_OBJECTS` each:
/// SQLite's compile-time ceiling `SQLITE_MAX_ATTACHED` (default 10, at most 125); the
/// connection's `SQLITE_LIMIT_ATTACHED` lowers it.
pub const SHARDS: usize = 125;

/// `shards`, lowered to the attached schemas the connection allows.
fn attach_limit(db: &Connection, shards: usize) -> usize {
    let limit = unsafe { rusqlite::ffi::sqlite3_limit(db.handle(), rusqlite::ffi::SQLITE_LIMIT_ATTACHED, -1) };
    shards.min(limit.max(0) as usize)
}

impl Sqlite {
    /// Installs `ir` into `db` and takes ownership of it. `Engine::install` opens an in-memory one.
    pub fn install_on(owned: Connection, ir: &Program) -> Result<Self, EngineError> {
        Self::install_on_with(owned, ir, 0)
    }

    /// `shards` > 0 attaches up to that many in-memory schemas, as the node tables need, and places
    /// node tables in them; the database then holds those tables only while this connection lives.
    pub fn install_on_with(owned: Connection, ir: &Program, shards: usize) -> Result<Self, EngineError> {
        Self::install_traced(owned, ir, shards, None)
    }

    /// `Engine::install` with work counting on: `work()` reads the counts.
    pub fn install_counted(ir: &Program) -> Result<Self, EngineError> {
        let db = Connection::open_in_memory().map_err(|e| error(Stage::Install, e))?;
        let work = crate::work::WorkLog::start(&db).map_err(|e| error(Stage::Install, e))?;
        Self::install_traced(db, ir, SHARDS, Some(work))
    }

    /// Counts since install, when counting.
    pub fn work(&self) -> Option<crate::work::Work> {
        self.work.as_ref().map(|work| work.work())
    }

    fn install_traced(owned: Connection, ir: &Program, shards: usize, mut work: Option<crate::work::WorkLog>) -> Result<Self, EngineError> {
        let db = &owned;
        let shards = attach_limit(db, shards);
        if ir.outputs.is_empty() {
            return Err(EngineError::new(
                Stage::Install,
                None,
                ErrorKind::Unsupported("output relation required"),
            ));
        }
        crate::terms::install(db, ir).map_err(|e| error(Stage::Install, e))?;
        // One schema read: the tables the host already holds. Every source DDL runs inside the
        // program install savepoint, ahead of the program DDL.
        let present: HashSet<String> = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .and_then(|mut statement| {
                statement.query_map([], |row| row.get::<_, String>(0))?.collect()
            })
            .map_err(|e| error(Stage::Install, e))?;
        let mut created = catalog::CreatedSources { shards, ..Default::default() };
        for source in ir.rels.iter().filter(|r| r.kind == RelKind::Source) {
            let columns = (0..source.cols.len())
                .map(|i| format!("c{i} INTEGER NOT NULL"))
                .collect::<Vec<_>>()
                .join(",");
            let keys = (0..source.cols.len()).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
            // A new host table is its own set: the key rejects a present insert.
            if present.contains(&source.name) {
                created.ddl.push_str(&format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {}({keys});",
                    catalog::quote(format!("ivm_host_{}_set", source.name)),
                    catalog::quote(&source.name),
                ));
            } else {
                created.ddl.push_str(&format!(
                    "CREATE TABLE {}({columns}, PRIMARY KEY ({keys})) WITHOUT ROWID;",
                    catalog::quote(&source.name)
                ));
                created.columns.insert(
                    source.name.clone(),
                    (0..source.cols.len()).map(|i| format!("c{i}")).collect(),
                );
            }
        }
        let mut programs = Vec::new();
        if ir.outputs.len() > 1 {
            let (bundle, members) = bundled_program(ir)?;
            let relation = bundle.rel(bundle.outputs[0]).expect("bundle output");
            let program =
                SqlProgram::install_ir_unwatched_terms_ready(db, &relation.name, &bundle, &created)
                    .map_err(plan_error)?;
            programs.push(OutputProgram { rel: relation.id, program, threshold: false, members });
        } else {
            for &output in &ir.outputs {
                let name = ir
                    .rel(output)
                    .map(|r| r.name.as_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("ivm_output_{output}"));
                let single = output_program(ir, output);
                let program =
                    SqlProgram::install_ir_unwatched_terms_ready(db, &name, &single, &created)
                        .map_err(plan_error)?;
                // The first install created the sources.
                created = catalog::CreatedSources { shards, ..Default::default() };
                let threshold = matches!(ir.strata.as_slice(), [Stratum::Let { id, body }]
                    if *id == output && matches!(ir.nodes.get(*body as usize), Some(Op::Threshold(_))));
                programs.push(OutputProgram {
                    rel: output,
                    program,
                    threshold,
                    members: Vec::new(),
                });
            }
        }
        if let Some(work) = &mut work { work.report("install"); }
        let engine = Self {
            work,
            db: owned,
            programs,
            ir: ir.clone(),
            tick: 0,
            counters: Counters::default(),
            marked: false,
        };
        // A frontier visits every installed output. Rusqlite's default cache of 16
        // statements evicts each output's SQL before the next frontier reaches it.
        let capacity = engine.statements().len() + ir.outputs.len() * 2 + ir.rels.len() * 3 + 16;
        engine.db.set_prepared_statement_cache_capacity(capacity);
        if !tracing::enabled!(target: "ivm_sqlite", tracing::Level::INFO) {
            return Ok(engine);
        }
        let schemas: Vec<String> = engine.db.prepare("SELECT name FROM pragma_database_list")
            .and_then(|mut list| list.query_map([], |row| row.get(0))?.collect())
            .unwrap_or_default();
        let creates_sql = schemas.iter()
            .map(|s| format!("(SELECT count(*) FROM {s}.sqlite_master WHERE sql LIKE 'CREATE %')"))
            .collect::<Vec<_>>().join(" + ");
        if let Ok(creates @ 1..) = engine.db.query_row(&format!("SELECT {creates_sql}"), [], |row| row.get::<_, i64>(0)) {
            // Columns of every table and view the install created: what projection narrows.
            let columns_sql = schemas.iter()
                .map(|s| format!("(SELECT count(*) FROM {s}.sqlite_master m JOIN pragma_table_info(m.name, '{s}') WHERE m.type IN ('table', 'view'))"))
                .collect::<Vec<_>>().join(" + ");
            let columns = engine.db.query_row(&format!("SELECT {columns_sql}"), [], |row| row.get::<_, i64>(0)).unwrap_or(-1);
            tracing::info!(target: "ivm_sqlite", sqlite_columns = columns, sqlite_create_count = creates, "ir schema");
        }
        Ok(engine)
    }
}

impl Engine for Sqlite {
    fn install(ir: &Program) -> Result<Self, EngineError> {
        let db = Connection::open_in_memory().map_err(|e| error(Stage::Install, e))?;
        let work = if crate::work::WorkLog::wanted() {
            Some(crate::work::WorkLog::start(&db).map_err(|e| error(Stage::Install, e))?)
        } else { None };
        Self::install_traced(db, ir, SHARDS, work)
    }

    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        if let Some(work) = &mut self.work { work.report("between"); }
        let db = &self.db;
        let before: i64 = db.query_row("SELECT count(*) FROM ivm_term", [], |r| r.get(0))
            .map_err(|e| error(Stage::Settle, e))?;
        let mut counters = Counters::measured();
        *counters.statements.as_mut().unwrap() += 1;
        db.execute_batch("SAVEPOINT ivm_engine_frontier;")
            .map_err(|e| error(Stage::Settle, e))?;
        *counters.statements.as_mut().unwrap() += 1;
        let mut run = || -> Result<Vec<(RelId, Row, W)>, EngineError> {
            // Per changed host row: its source name (borrowed from the IR), sign and cells.
            let mut host: Vec<(&str, Sign, Vec<Cell>)> = Vec::new();
            // Per source: its insert and its delete, built on first use.
            let mut statements: std::collections::HashMap<RelId, (String, String)> = std::collections::HashMap::new();
            for change in &frontier.changes {
                let source = self
                    .ir
                    .rel(change.rel)
                    .filter(|r| r.kind == RelKind::Source)
                    .ok_or_else(|| {
                        EngineError::new(
                            Stage::Settle,
                            Some(change.rel),
                            ErrorKind::UnknownRel(change.rel),
                        )
                    })?;
                if change.row.len() != source.cols.len() {
                    return Err(EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::Arity {
                            expected: source.cols.len(),
                            actual: change.row.len(),
                        },
                    ));
                }
                if change.w != 1 && change.w != -1 {
                    return Err(EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::Unsupported("weight other than +1/-1"),
                    ));
                }
                let (insert, delete) = statements.entry(change.rel).or_insert_with(|| {
                    let names = self.programs.iter().find_map(|output| {
                        output.program.inner.plan.scans.iter().find(|scan| scan.table == source.name)
                            .map(|scan| scan.columns.clone())
                    }).unwrap_or_else(|| (0..source.cols.len()).map(|i| format!("c{i}")).collect());
                    let match_row = names
                        .iter()
                        .map(|c| format!("{}=?", catalog::quote(c)))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    let table = catalog::quote(&source.name);
                    (
                        format!("INSERT INTO {table} VALUES ({})", vec!["?"; names.len()].join(",")),
                        format!("DELETE FROM {table} WHERE {match_row}"),
                    )
                });
                // The source's unique index rejects a present insert; a delete of an absent
                // row matches nothing and changes nothing.
                let sql = if change.w > 0 { &*insert } else { &*delete };
                let changed = match db.prepare_cached(sql)
                    .and_then(|mut stmt| stmt.execute(rusqlite::params_from_iter(&change.row)))
                {
                    Err(rusqlite::Error::SqliteFailure(failure, _))
                        if change.w > 0 && failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        return Err(EngineError::new(
                            Stage::Settle,
                            Some(change.rel),
                            ErrorKind::PresentInsert(change.row.clone()),
                        ));
                    }
                    changed => changed.map_err(|e| error(Stage::Settle, e))?,
                };
                *counters.statements.as_mut().unwrap() += 1;
                if changed == 0 {
                    continue;
                }
                host.push((
                    source.name.as_str(),
                    if change.w > 0 { Sign::Insert } else { Sign::Delete },
                    change.row.iter().copied().map(Cell::Integer).collect(),
                ));
            }
            let batch: Vec<crate::engine::Staged<'_>> = host.iter()
                .map(|(relation, sign, row)| crate::engine::Staged { relation, sign: *sign, row })
                .collect();
            let mut changes = Vec::new();
            for output in &self.programs {
                let sources = &output.program.inner.plan.sources;
                let mut used = batch.iter().filter(|change| sources.iter().any(|source| source == change.relation)).copied().collect::<Vec<_>>();
                let mut terms = if output.members.is_empty() { 0 } else {
                    *counters.statements.as_mut().unwrap() += 1;
                    db.query_row("SELECT count(*) FROM ivm_term", [], |r| r.get::<_, i64>(0))
                        .map_err(|e| error(Stage::Settle, e))?
                };
                // Later strata and recursive rounds can mint rows after an earlier
                // constructor scan. Empty-source passes carry those rows forward.
                for pass in 0..=8192 {
                    let (rows, work) = crate::engine::settle_rows(db, &output.program.inner, &used)
                        .map_err(|e| match e.kind {
                            crate::ErrorKind::LetRecLimit { rel, limit } => EngineError::new(Stage::Settle, rel, ErrorKind::LetRecLimit(limit)),
                            _ => error(Stage::Settle, e),
                        })?;
                    for (target, source) in [
                        (&mut counters.delta_rows.filter, work.delta_rows.filter),
                        (&mut counters.delta_rows.join, work.delta_rows.join),
                        (&mut counters.delta_rows.antijoin, work.delta_rows.antijoin),
                        (&mut counters.delta_rows.reduce, work.delta_rows.reduce),
                        (&mut counters.delta_rows.topk, work.delta_rows.topk),
                        (&mut counters.delta_rows.window, work.delta_rows.window),
                        (&mut counters.delta_rows.mint, work.delta_rows.mint),
                    ] {
                        match (target.as_mut(), source) {
                            (Some(target), Some(source)) => *target += source,
                            (_, None) => *target = None,
                            _ => {}
                        }
                    }
                    *counters.rounds.as_mut().unwrap() += work.rounds.unwrap_or(0);
                    *counters.statements.as_mut().unwrap() += work.statements.unwrap_or(0);
                    // The plan's output rows, consolidated by `output_delta`: a threshold output
                    // reports each row's sign, any other its weight; a bundle row leads with its
                    // member relation.
                    for (row, weight) in rows {
                        let weight = if output.threshold { if weight < 0 { -1 } else { 1 } } else { weight };
                        if output.members.is_empty() {
                            changes.push((output.rel, row, weight));
                        } else {
                            let rel = row[0] as RelId;
                            let width = output.members.iter().find(|(id, _)| *id == rel)
                                .map(|(_, width)| *width)
                                .ok_or_else(|| error(Stage::Settle, "unknown bundled output"))?;
                            changes.push((rel, row[1..=width].to_vec(), weight));
                        }
                    }
                    if output.members.is_empty() { break; }
                    *counters.statements.as_mut().unwrap() += 1;
                    let after: i64 = db.query_row("SELECT count(*) FROM ivm_term", [], |r| r.get(0))
                        .map_err(|e| error(Stage::Settle, e))?;
                    if after == terms { break; }
                    if pass == 8192 {
                        return Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("constructor closure budget")));
                    }
                    terms = after;
                    used.clear();
                }
            }
            changes.sort();
            let mut consolidated: Vec<(RelId, Row, W)> = Vec::new();
            for (rel, row, weight) in changes {
                if let Some((last_rel, last_row, last_weight)) = consolidated.last_mut() {
                    if *last_rel == rel && *last_row == row {
                        *last_weight = last_weight.checked_add(weight)
                            .ok_or_else(|| error(Stage::Settle, "output weight overflow"))?;
                        continue;
                    }
                }
                consolidated.push((rel, row, weight));
            }
            consolidated.retain(|(_, _, weight)| *weight != 0);
            Ok(consolidated)
        };
        match run() {
            Ok(changes) => {
                let after: i64 = match db.query_row("SELECT count(*) FROM ivm_term", [], |r| r.get(0)) {
                    Ok(after) => after,
                    Err(e) => {
                        let _ = db.execute_batch("ROLLBACK TO ivm_engine_frontier; RELEASE ivm_engine_frontier;");
                        return Err(error(Stage::Settle, e));
                    }
                };
                *counters.statements.as_mut().unwrap() += 1;
                db.execute_batch("RELEASE ivm_engine_frontier;")
                    .map_err(|e| error(Stage::Settle, e))?;
                *counters.statements.as_mut().unwrap() += 1;
                counters.interned = Some((after - before) as u64);
                counters.rows_written = changes.len() as u64;
                self.counters = counters;
                let tick = self.tick;
                self.tick += 1;
                if let Some(work) = &mut self.work { work.report("settle"); }
                Ok(Delta { tick, changes })
            }
            Err(e) => {
                let _ = db
                    .execute_batch("ROLLBACK TO ivm_engine_frontier; RELEASE ivm_engine_frontier;");
                if let Some(work) = &mut self.work { work.report("settle"); }
                Err(e)
            }
        }
    }

    fn counters(&self) -> Counters { self.counters }

    fn rewinds(&self) -> bool { true }

    /// One savepoint around everything after the mark. Each settle's `ivm_engine_frontier`
    /// savepoint nests inside it; rewinding is SQLite restoring the journaled pages, with no
    /// statement per relation.
    fn mark(&mut self) -> Result<(), EngineError> {
        let sql = if self.marked {
            "RELEASE ivm_engine_mark; SAVEPOINT ivm_engine_mark;"
        } else {
            "SAVEPOINT ivm_engine_mark;"
        };
        self.db.execute_batch(sql).map_err(|e| error(Stage::Settle, e))?;
        self.marked = true;
        Ok(())
    }

    fn rewind(&mut self) -> Result<(), EngineError> {
        if !self.marked {
            return Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("rewind without mark")));
        }
        self.db.execute_batch("ROLLBACK TO ivm_engine_mark;").map_err(|e| error(Stage::Settle, e))
    }

    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let output = self
            .programs
            .iter()
            .find(|output| output.rel == rel || output.members.iter().any(|(id, _)| *id == rel))
            .ok_or_else(|| {
                EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel))
            })?;
        let db = &self.db;
        let plan = &output.program.inner.plan;
        let width = plan.output.len();
        let crate::plan::Root::Nodes(nodes) = &plan.root else { unreachable!() };
        if output.members.is_empty() {
            let mut stmt = db.prepare_cached(&nodes.output_snapshot).map_err(|e| error(Stage::Snapshot, e))?;
            let result = stmt
                .query_map([], |r| Ok((row_of(r, width)?, r.get::<_, W>(width)?)))
                .map_err(|e| error(Stage::Snapshot, e))?
                .map(|r| r.map_err(|e| error(Stage::Snapshot, e)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(result.into_iter().map(|(row, weight)| (row, if output.threshold { weight.min(1) } else { weight })).collect())
        } else {
            // The bundle's integrated table is keyed by its columns, the member id first:
            // one member is one index range.
            let arity = output.members.iter().find(|(id, _)| *id == rel).expect("selected member").1;
            let mut stmt = db.prepare_cached(&nodes.member_snapshot).map_err(|e| error(Stage::Snapshot, e))?;
            let rows = stmt.query_map([rel as i64], |r| Ok(((1..=arity).map(|i| r.get(i)).collect::<Result<Row, _>>()?, r.get::<_, W>(width)?)))
                .map_err(|e| error(Stage::Snapshot, e))?
                .map(|r| r.map_err(|e| error(Stage::Snapshot, e)))
                .collect();
            rows
        }
    }

    fn intern_snapshot(&self, functor: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let db = &self.db;
        crate::terms::snapshot(db, &self.ir, functor).map_err(|e| error(Stage::Snapshot, e))
    }
    fn declare_constructors(&mut self, ctors: &[(String, Vec<ivm_ir::Ty>)]) -> Result<Vec<RelId>, EngineError> {
        let mut next = self.ir.rels.iter().map(|rel| rel.id).chain(self.programs.iter().map(|p| p.rel)).max().map_or(0, |id| id + 1);
        let (mut ids, mut added) = (Vec::with_capacity(ctors.len()), Vec::new());
        for (name, types) in ctors {
            let known = self.ir.rels.iter().chain(&added).find(|rel: &&ivm_ir::Relation| rel.kind == RelKind::Constructor && &rel.name == name);
            if let Some(rel) = known {
                ids.push(rel.id);
                continue;
            }
            let mut cols = vec![ivm_ir::Ty::Id];
            cols.extend(types.iter().copied());
            added.push(ivm_ir::Relation { id: next, name: name.clone(), cols, kind: RelKind::Constructor });
            ids.push(next);
            next += 1;
        }
        if !added.is_empty() {
            let declared = Program { texts: Vec::new(), rels: added.clone(), nodes: Vec::new(), strata: Vec::new(), outputs: Vec::new() };
            crate::terms::install(&self.db, &declared).map_err(|e| error(Stage::Settle, e))?;
            self.ir.rels.extend(added);
        }
        Ok(ids)
    }
    fn intern_terms(&mut self, terms: &[(RelId, Row)]) -> Result<Vec<i64>, EngineError> {
        let named = terms.iter().map(|(functor, args)| {
            let rel = self.ir.rel(*functor).filter(|rel| rel.kind == RelKind::Constructor)
                .ok_or_else(|| EngineError::new(Stage::Settle, Some(*functor), ErrorKind::UnknownRel(*functor)))?;
            if args.len() + 1 != rel.cols.len() {
                return Err(EngineError::new(Stage::Settle, Some(*functor), ErrorKind::Arity { expected: rel.cols.len() - 1, actual: args.len() }));
            }
            Ok((rel.name.as_str(), args.as_slice()))
        }).collect::<Result<Vec<_>, _>>()?;
        crate::terms::intern_terms(&self.db, &named).map_err(|e| error(Stage::Settle, e))
    }
    fn intern_text(&mut self, value: &str) -> Result<i64, EngineError> {
        let db = &self.db;
        crate::terms::intern_text(db, value).map_err(|e| error(Stage::Settle, e))
    }
    fn text(&self, id: i64) -> Result<Option<String>, EngineError> {
        let db = &self.db;
        crate::terms::text(db, id).map_err(|e| error(Stage::Snapshot, e))
    }
    fn intern_any(&mut self, value: &ivm_ir::AnyValue) -> Result<i64, EngineError> {
        let db = &self.db;
        crate::terms::intern_any_value(db, value).map_err(|e| error(Stage::Settle, e))
    }
    fn any_value(&self, id: i64) -> Result<ivm_ir::AnyValue, EngineError> {
        let db = &self.db;
        crate::terms::any_value(db, id).map_err(|e| error(Stage::Snapshot, e))
    }
}
