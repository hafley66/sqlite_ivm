//! Storage layout and program lifecycle.
//!
//! Per installed program `P` (names validated identifiers; engine objects are
//! prefixed `frontier_`):
//!
//! | object | shape |
//! | --- | --- |
//! | `frontier_catalog` | `name TEXT PK, program TEXT, frontier INTEGER, install INTEGER` |
//! | `frontier_catalog_column` | `name, pos, col` — output schema rows, no JSON |
//! | `frontier_P_stage` | `__seq, __table, __sign, v0..vW-1` — the transaction's collected batch |
//! | `frontier_P_s<i>` | one per scan: netted signed delta over the scan's needed columns |
//! | `frontier_P_j<i>` | one per join: net signed derivation delta over the join output |
//! | `frontier_P_root` | union root: output key columns + `__weight`; group root: keys + `__n` + `__s<i>` |
//! | `frontier_P_touch` | before-images of the rows this frontier touches |
//! | `frontier_P_delta` | `__sign` + output columns: the last settled frontier's net change |
//! | `frontier_P` | view over the root: visible rows only |
//! | `frontier_P_x<i>` | index on a source table's join-key columns |
//! | `frontier_P_rootk` | unique index over the root key columns |
//!
//! Value columns carry no declared type: no affinity rewrites a storage class
//! on the way in, so cells stay exactly as the source wrote them. Row identity
//! is the tables' integer rowids; every lookup below runs through the unique
//! or source indexes.
//!
//! Install back-fills the root from the rows the sources already hold — one
//! set-wise read per source, explicit `Unsupported` on NULL or non-numeric
//! group-sum storage. The initial delta stays empty and the frontier counter
//! stays 0; the first committed source transaction is frontier 1.

use crate::error::{EngineError, ErrorKind, Stage};
use crate::meter::Meter;
use crate::observe;
use crate::plan::{self, Compiled, Root, ScanSpec};
use crate::OutputColumn;
use ivm_ir::{Program as IrProgram, Ty};
use sqlite_ext::rusqlite::{types::Value, Connection};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct Installed {
    pub name: String,
    pub ir: IrProgram,
    pub sql_text: bool,
    pub install: u64,
    pub plan: Compiled,
    /// Output schema, in output order.
    pub output: Vec<OutputColumn>,
    /// Watched source tables, first-use order.
    pub sources: Vec<String>,
    /// The watched sources that are other programs' output views. Their
    /// indexes are skipped: a view cannot carry one.
    pub derived: Vec<String>,
    /// Every statement one settle issues, pregenerated at install.
    pub sqls: SettleSql,
}

/// The per-frontier statement set. Built once: every settle issues the same
/// text, so `prepare_cached` pays once and repeated frontiers prepare nothing.
pub(crate) struct SettleSql {
    pub clears: Vec<(String, String)>,
    pub stage_insert: String,
    pub bump: String,
    pub read_frontier: String,
    pub read_delta: String,
    pub weight_delta: Option<String>,
    pub snapshot: String,
}

// ---------------------------------------------------------------------------
// Names

pub(crate) fn catalog() -> &'static str {
    "frontier_catalog"
}

pub(crate) fn catalog_column() -> &'static str {
    "frontier_catalog_column"
}

pub(crate) fn stage(p: &str) -> String {
    format!("frontier_{p}_stage")
}

pub(crate) fn scan_stage(p: &str, i: usize) -> String {
    format!("frontier_{p}_s{i}")
}

pub(crate) fn join_delta(p: &str, i: usize) -> String {
    format!("frontier_{p}_j{i}")
}

pub(crate) fn anti_delta(p: &str, i: usize) -> String {
    format!("frontier_{p}_a{i}")
}

pub(crate) fn topk_delta(p: &str, i: usize) -> String {
    format!("frontier_{p}_k{i}")
}

pub(crate) fn root(p: &str) -> String {
    format!("frontier_{p}_root")
}

pub(crate) fn touch(p: &str) -> String {
    format!("frontier_{p}_touch")
}

pub(crate) fn delta(p: &str) -> String {
    format!("frontier_{p}_delta")
}

pub(crate) fn view(p: &str) -> String {
    format!("frontier_{p}")
}

pub(crate) fn source_index(p: &str, i: usize) -> String {
    format!("frontier_{p}_x{i}")
}

pub(crate) fn collector(p: &str, install: u64) -> String {
    format!("frontier_{p}_c{install}")
}

/// Double-quoted SQL identifier.
pub(crate) fn quote(s: impl AsRef<str>) -> String {
    format!("\"{}\"", s.as_ref().replace('"', "\"\""))
}

pub(crate) fn decode_sql(ty: Ty, value: &str) -> String {
    match ty {
        Ty::Real => format!("ivm_real_value({value})"),
        Ty::Text => format!("ivm_text_value({value})"),
        Ty::Any => format!("ivm_any_value({value})"),
        Ty::Int | Ty::Id => value.to_owned(),
    }
}

fn validate_program_name(name: &str) -> Result<(), EngineError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.to_ascii_lowercase().starts_with("sqlite_");
    if ok {
        Ok(())
    } else {
        Err(EngineError::new(
            Stage::Install,
            name,
            ErrorKind::State(
                "program names must be plain identifiers outside the sqlite_ space".into(),
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// Schema reads

pub(crate) fn table_columns(
    conn: &Connection,
    table: &str,
    meter: &mut Meter,
) -> Result<Vec<String>, EngineError> {
    let sql = format!("PRAGMA table_info({})", quote(table));
    let rows: Vec<String> = meter
        .rows(conn, "install", table, &sql, [], |row| {
            row.get::<_, String>(1)
        })
        .map_err(|e| EngineError::new(Stage::Plan, table, ErrorKind::Sqlite(e.to_string())))?;
    Ok(rows)
}

/// The program whose output view `table` names, if any: the catalog maps
/// `frontier_<name>` to its installed program.
pub(crate) fn derived_view_program(
    conn: &Connection,
    table: &str,
) -> Result<Option<String>, EngineError> {
    // Callers guarantee the catalog exists: install creates it before this
    // runs, and open only reads installed programs.
    let sql = format!(
        "SELECT name FROM {} WHERE 'frontier_' || name = ?1",
        quote(catalog())
    );
    match conn.query_row(&sql, [table], |row| row.get::<_, String>(0)) {
        Ok(program) => Ok(Some(program)),
        Err(sqlite_ext::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(EngineError::new(
            Stage::Install,
            table,
            ErrorKind::Sqlite(e.to_string()),
        )),
    }
}

/// Which of an install's sources cannot carry a source index: other
/// programs' output views and plain user views. Recorded on the install so
/// program DDL and teardown skip exactly the same index statements.
pub(crate) fn scan_derived_sources(
    conn: &Connection,
    inst: &Installed,
) -> Result<Vec<String>, EngineError> {
    scan_derived_sources_created(conn, inst, &CreatedSources::default())
}

/// `scan_derived_sources`, skipping the base tables the install creates.
fn scan_derived_sources_created(
    conn: &Connection,
    inst: &Installed,
    created: &CreatedSources,
) -> Result<Vec<String>, EngineError> {
    let mut derived = Vec::new();
    for source in &inst.sources {
        if created.columns.contains_key(source) {
            continue;
        }
        if derived_view_program(conn, source)?.is_some() || !is_base_table(conn, source)? {
            derived.push(source.clone());
        }
    }
    Ok(derived)
}

/// Whether the relation is a real table in `main` (the only schema the
/// extension watches). Views of any kind answer false.
pub(crate) fn is_base_table(conn: &Connection, name: &str) -> Result<bool, EngineError> {
    match conn.query_row(
        "SELECT type FROM sqlite_master WHERE name = ?1",
        [name],
        |row| row.get::<_, String>(0),
    ) {
        Ok(kind) => Ok(kind == "table"),
        Err(sqlite_ext::rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(EngineError::new(
            Stage::Install,
            name,
            ErrorKind::Sqlite(e.to_string()),
        )),
    }
}

// ---------------------------------------------------------------------------
// Install

/// Whether install registers the commit collector over the program's source
/// tables. A composed program settles through
/// [`crate::Composition::settle`](crate::Composition) instead, so its sources
/// — another program's output view — get no collector (a view cannot carry
/// the AFTER triggers, and a trigger firing inside another program's `xSync`
/// would hit SQLite's `SQLITE_LOCKED` on virtual-table writes during sync).
pub(crate) enum Watch {
    /// Watch the program's source tables; SQL writes settle at `COMMIT`.
    Sources,
    /// No collector: the program settles only through the explicit API.
    None,
}

/// Base tables an install creates inside its own savepoint, before the program DDL: their
/// `CREATE` statements and the columns of each, known without a schema read.
#[derive(Default)]
pub(crate) struct CreatedSources {
    pub(crate) ddl: String,
    pub(crate) columns: HashMap<String, Vec<String>>,
}

pub(crate) fn install(
    conn: &Connection,
    name: &str,
    select_sql: &str,
    watch: Watch,
) -> Result<Arc<Installed>, EngineError> {
    let parsed = compile(conn, name, select_sql)?;
    let program = plan::lower_ir(&parsed, &|table| source_types(conn, table))?;
    install_program(conn, name, &program, parsed.output, watch, false, false, &CreatedSources::default())
}

pub(crate) fn install_ir(
    conn: &Connection,
    name: &str,
    program: &IrProgram,
    watch: Watch,
    terms_ready: bool,
    created: &CreatedSources,
) -> Result<Arc<Installed>, EngineError> {
    let output_id = *program.outputs.first().ok_or_else(|| {
        EngineError::unsupported(Stage::Plan, name, "a typed program needs one output")
    })?;
    let width = program
        .rel(output_id)
        .ok_or_else(|| EngineError::unsupported(Stage::Plan, name, "output relation is missing"))?
        .cols
        .len();
    let output = (0..width)
        .map(|i| OutputColumn {
            name: format!("c{i}"),
        })
        .collect();
    install_program(conn, name, program, output, watch, true, terms_ready, created)
}

fn install_program(
    conn: &Connection,
    name: &str,
    program: &IrProgram,
    output: Vec<OutputColumn>,
    watch: Watch,
    typed_ir: bool,
    terms_ready: bool,
    created: &CreatedSources,
) -> Result<Arc<Installed>, EngineError> {
    validate_program_name(name)?;
    let _guard =
        tracing::info_span!(target: observe::TARGET, observe::INSTALL_SPAN, program = name)
            .entered();
    let compiled = compile_ir(conn, name, program, output, typed_ir, &created.columns)?;
    let mut meter = Meter::default();

    if catalog_row(conn, name, &mut meter)?.is_some() {
        return Err(EngineError::new(
            Stage::Install,
            name,
            ErrorKind::State("a program with this name is already installed".into()),
        ));
    }
    let install = next_install(conn, &mut meter)?;
    let installed = {
        let mut built = build_installed(name, install, compiled, program.clone(), !typed_ir);
        built.derived = scan_derived_sources_created(conn, &built, created)?;
        if matches!(watch, Watch::Sources) && !built.derived.is_empty() {
            return Err(EngineError::unsupported(
                Stage::Install,
                built.derived[0].clone(),
                "a view is not a settlement source; sources must be base tables — install the producer-consumer pair through Composition",
            ));
        }
        built
    };

    let mut sql = String::new();
    push_savepoint(&mut sql, "frontier_sp_install");
    sql.push_str(&created.ddl);
    push_catalog_ddl(&mut sql);
    push_program_ddl(&installed, &mut sql);
    // Persist the typed program; SQL text is only an install-time input.
    let program_json = serde_json::to_string(&program)
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::State(e.to_string())))?;
    sql.push_str(&format!(
        "INSERT INTO {c}(name, program, frontier, install, typed_ir) VALUES ('{n}', '{s}', 0, {i}, {});",
        typed_ir as u8,
        c = quote(catalog()),
        n = name.replace('\'', "''"),
        s = program_json.replace('\'', "''"),
        i = install,
    ));
    for (pos, col) in installed.output.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO {cc}(name, pos, col) VALUES ('{n}', {pos}, '{col}');",
            cc = quote(catalog_column()),
            n = name.replace('\'', "''"),
            col = col.name.replace('\'', "''"),
        ));
    }
    meter
        .batch(conn, "install", name, &sql)
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    if !terms_ready {
        if let Err(e) = crate::terms::install(conn, program) {
            rollback(conn, "frontier_sp_install");
            return Err(EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())));
        }
    }
    if let Err(e) = bootstrap(conn, &installed, &mut meter) {
        rollback(conn, "frontier_sp_install");
        return Err(e);
    }
    let arc = Arc::new(installed);
    if matches!(watch, Watch::Sources) {
        if let Err(e) = crate::engine::watch_collector(conn, &arc) {
            rollback(conn, "frontier_sp_install");
            return Err(e);
        }
    }
    release(conn, "frontier_sp_install");
    Ok(arc)
}
/// Back-fills the root from the rows the sources already hold, so the
/// installed snapshot equals a fresh evaluation of the defining SELECT.
/// Entirely set-wise: one `INSERT .. SELECT` per distinct source reads each
/// source exactly once (a composed consumer stages from the producer's
/// already-bootstrapped view), then the pregenerated settle statements net
/// the staged rows through the scan/join deltas into the root. No frontier
/// occurs: the catalog row keeps `frontier = 0`, the delta table stays
/// empty, and nothing is emitted. The guards fail the install explicitly
/// when a source holds cells the engine cannot settle exactly (`NULL`
/// anywhere in a staged row; non-numeric storage in a group-sum column) —
/// the same contract `validate_batch` enforces on every later settle. A
/// failure propagates to [`install`], which rolls the install savepoint
/// back: no catalog row, no collector, no shadow objects, sources untouched.
fn bootstrap(conn: &Connection, inst: &Installed, meter: &mut Meter) -> Result<(), EngineError> {
    let phase = "install";
    let p = &inst.name;

    // Typed IR has no constant-producing node, so empty source tables leave
    // every node empty. Check sources in batches before preparing fill SQL.
    if !inst.sql_text && !inst.plan.sources.is_empty() {
        let mut has_rows = false;
        for tables in inst.plan.sources.chunks(128) {
            let query = format!("SELECT {}", tables.iter().map(|table| {
                format!("EXISTS(SELECT 1 FROM {} LIMIT 1)", quote(table))
            }).collect::<Vec<_>>().join(" OR "));
            has_rows = conn.query_row(&query, [], |row| row.get::<_, bool>(0))
                .map_err(|e| EngineError::new(Stage::Install, p, ErrorKind::Sqlite(e.to_string())))?;
            if has_rows { break; }
        }
        if !has_rows { return Ok(()); }
    }

    // Stage every current source row with sign +1, in declared column
    // order. The staging columns carry no declared type, so cells keep
    // their storage classes end to end.
    let mut sql = String::new();
    for table in &inst.plan.sources {
        let scan = source_scan(inst, table);
        let into: Vec<String> = (0..scan.columns.len()).map(|i| format!("v{i}")).collect();
        let cols: Vec<String> = scan.columns.iter().map(|c| quote(c)).collect();
        sql.push_str(&format!(
            "INSERT INTO {t}(__seq, __table, __sign, {into}) \
             SELECT 0, '{lit}', 1, {cols} FROM {src};",
            t = quote(stage(p)),
            into = into.join(","),
            cols = cols.join(","),
            lit = table.replace('\'', "''"),
            src = quote(table),
        ));
    }
    meter
        .batch(conn, phase, p, &sql)
        .map_err(|e| EngineError::new(Stage::Install, p, ErrorKind::Sqlite(e.to_string())))?;

    // The guards read the staged rows, not the sources: they see exactly
    // what the fills below will consume.
    let sum_positions: Vec<(String, usize)> = match &inst.plan.root {
        Root::Group { scan, sums, .. } => {
            let scan = &inst.plan.scans[*scan];
            sums.iter()
                .map(|s| {
                    let col = &scan.needed[s.take];
                    let pos = scan
                        .columns
                        .iter()
                        .position(|c| c == col)
                        .expect("needed columns come from the declared list");
                    (scan.table.clone(), pos)
                })
                .collect()
        }
        Root::Nodes(_) => inst.ir.nodes.iter().filter_map(|op| {
            let ivm_ir::Op::Reduce { input, aggs, .. } = op else { return None; };
            let ivm_ir::Op::Mfp { input: get, project, .. } = inst.ir.nodes.get(*input as usize)? else { return None; };
            let ivm_ir::Op::Get(rel) = inst.ir.nodes.get(*get as usize)? else { return None; };
            let name = inst.ir.rel(*rel)?.name.clone();
            Some(aggs.iter().filter_map(|agg| {
                let ivm_ir::Agg::Sum(col) = agg else { return None; };
                Some((name.clone(), project.get(*col as usize).copied().unwrap_or(*col) as usize))
            }).collect::<Vec<_>>())
        }).flatten().collect(),
        Root::Union { .. } => Vec::new(),
    };
    for table in &inst.plan.sources {
        if inst.sql_text { continue; }
        let scan = source_scan(inst, table);
        let nulls = (0..scan.columns.len())
            .map(|i| format!("v{i} IS NULL"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut filters = vec![format!("count(*) FILTER (WHERE {nulls})")];
        for (sum_table, pos) in &sum_positions {
            if sum_table == table {
                filters.push(format!(
                    "count(*) FILTER (WHERE typeof(v{pos}) NOT IN ('integer','real'))"
                ));
            }
        }
        let sql = format!(
            "SELECT {filters} FROM {t} WHERE __table = '{lit}';",
            filters = filters.join(","),
            t = quote(stage(p)),
            lit = table.replace('\'', "''"),
        );
        let width = filters.len();
        let Some(counts) = meter
            .one(conn, phase, table, &sql, [], |row| {
                (0..width)
                    .map(|i| row.get::<_, i64>(i))
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| {
                EngineError::new(Stage::Install, table, ErrorKind::Sqlite(e.to_string()))
            })?
        else {
            return Err(EngineError::new(
                Stage::Install,
                table,
                ErrorKind::Sqlite("bootstrap guard read failed".into()),
            ));
        };
        if counts[0] > 0 {
            return Err(EngineError::unsupported(
                Stage::Install,
                table,
                "pre-existing rows hold NULL cells; keys and sums over NULL are not a supported shape",
            ));
        }
        if counts[1..].iter().any(|&c| c > 0) {
            return Err(EngineError::unsupported(
                Stage::Install,
                table,
                "pre-existing group-sum cells hold TEXT/BLOB storage; only INTEGER and REAL settle exactly",
            ));
        }
    }

    // Net the staged rows through the engine's own fill statements.
    if let Root::Nodes(nodes) = &inst.plan.root {
        if inst.sql_text { encode_stage(conn, inst)?; }
        nodes
            .run(conn, &mut ivm_engine::Counters::measured())
            .map_err(|e| EngineError::new(Stage::Install, p, ErrorKind::Sqlite(e.to_string())))?;
        conn.execute(&format!("DELETE FROM {}", quote(stage(p))), [])
            .map_err(|e| EngineError::new(Stage::Install, p, ErrorKind::Sqlite(e.to_string())))?;
        return Ok(());
    }
    unreachable!("runtime plans always use NodesPlan")
}

pub(crate) fn encode_stage(conn: &Connection, inst: &Installed) -> Result<(), EngineError> {
    let width = inst.plan.stage_width;
    let table = quote(stage(&inst.name));
    let vals = (0..width).map(|i| format!("v{i}")).collect::<Vec<_>>().join(",");
    let query = format!("SELECT rowid,__table,{vals} FROM {table}");
    let mut stmt = conn.prepare(&query).map_err(|e| EngineError::new(Stage::Settle, &inst.name, ErrorKind::Sqlite(e.to_string())))?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?,
            (0..width).map(|i| row.get::<_, Value>(i + 2)).collect::<Result<Vec<_>, _>>()?))
    }).map_err(|e| EngineError::new(Stage::Settle, &inst.name, ErrorKind::Sqlite(e.to_string())))?
        .collect::<Result<Vec<_>, _>>().map_err(|e| EngineError::new(Stage::Settle, &inst.name, ErrorKind::Sqlite(e.to_string())))?;
    drop(stmt);
    for (rowid, source, values) in rows {
        let rel = inst.ir.rels.iter().find(|r| r.kind == ivm_ir::RelKind::Source && r.name == source)
            .ok_or_else(|| EngineError::unsupported(Stage::Settle, &source, "source relation missing"))?;
        let encoded = rel.cols.iter().zip(&values).map(|(ty, value)| {
            crate::terms::encode_value(conn, *ty, value)
                .map_err(|e| EngineError::new(Stage::Settle, &source, ErrorKind::Sqlite(e.to_string())))
        }).collect::<Result<Vec<_>, _>>()?;
        let assigns = (0..encoded.len()).map(|i| format!("v{i}=?{}", i + 1)).collect::<Vec<_>>().join(",");
        let update = format!("UPDATE {table} SET {assigns} WHERE rowid=?{}", encoded.len() + 1);
        conn.execute(&update, sqlite_ext::rusqlite::params_from_iter(encoded.into_iter().chain([rowid])))
            .map_err(|e| EngineError::new(Stage::Settle, &source, ErrorKind::Sqlite(e.to_string())))?;
    }
    Ok(())
}

/// The plan's scan over one distinct source table.
fn source_scan<'a>(inst: &'a Installed, table: &str) -> &'a ScanSpec {
    inst.plan
        .scans
        .iter()
        .find(|s| s.table == table)
        .expect("every plan source has a scan")
}

fn compile(conn: &Connection, name: &str, select_sql: &str) -> Result<Compiled, EngineError> {
    let columns = |table: &str| -> Option<Vec<String>> {
        let mut fresh = Meter::default();
        table_columns(conn, table, &mut fresh)
            .ok()
            .filter(|cols| !cols.is_empty())
    };
    plan::compile(name, select_sql, &columns)
}

fn source_types(conn: &Connection, table: &str) -> Option<Vec<Ty>> {
    if let Some(program_name) = table.strip_prefix("frontier_") {
        if let Ok(json) = conn.query_row(
            "SELECT program FROM frontier_catalog WHERE name=?1", [program_name],
            |row| row.get::<_, String>(0),
        ) {
            let ir: IrProgram = serde_json::from_str(&json).ok()?;
            return Some(ir.rel(*ir.outputs.first()?)?.cols.clone());
        }
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quote(table))).ok()?;
    let declared = stmt.query_map([], |row| Ok((row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(5)?))).ok()?
        .collect::<Result<Vec<_>, _>>().ok()?;
    if declared.is_empty() { return None; }
    Some(declared.into_iter().map(|(name, not_null, primary_key)| {
        if not_null == 0 && primary_key == 0 { return Ty::Any; }
        let name = name.to_ascii_uppercase();
        if name.contains("INT") { Ty::Int }
        else if name.contains("CHAR") || name.contains("CLOB") || name.contains("TEXT") { Ty::Text }
        else if name.contains("REAL") || name.contains("FLOA") || name.contains("DOUB") { Ty::Real }
        else { Ty::Any }
    }).collect())
}

fn compile_ir(
    conn: &Connection,
    name: &str,
    program: &IrProgram,
    output: Vec<OutputColumn>,
    typed_ir: bool,
    created: &HashMap<String, Vec<String>>,
) -> Result<Compiled, EngineError> {
    let columns = |table: &str| -> Option<Vec<String>> {
        if let Some(columns) = created.get(table) {
            return Some(columns.clone());
        }
        let mut fresh = Meter::default();
        table_columns(conn, table, &mut fresh)
            .ok()
            .filter(|cols| !cols.is_empty())
    };
    let compiled = plan::compile_ir(name, program, output, &columns, typed_ir)?;
    if let Root::Nodes(nodes) = &compiled.root {
        nodes.register_work(conn)
            .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    }
    Ok(compiled)
}

fn catalog_row(
    conn: &Connection,
    name: &str,
    meter: &mut Meter,
) -> Result<Option<(String, u64, bool)>, EngineError> {
    let sql = format!(
        "SELECT program, install, typed_ir FROM {} WHERE name = ?1",
        quote(catalog())
    );
    // First install on a database has no catalog yet; create it before reads.
    meter
        .batch(conn, "install", name, &format!(
            "CREATE TABLE IF NOT EXISTS {}(name TEXT PRIMARY KEY, program TEXT NOT NULL, frontier INTEGER NOT NULL DEFAULT 0, install INTEGER NOT NULL, typed_ir INTEGER NOT NULL DEFAULT 0);\
             CREATE TABLE IF NOT EXISTS {}(name TEXT NOT NULL, pos INTEGER NOT NULL, col TEXT NOT NULL, PRIMARY KEY(name, pos))",
            quote(catalog()),
            quote(catalog_column()),
        ))
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    migrate_legacy_catalog(conn, meter)?;
    let columns: Vec<String> = meter.rows(conn, "install", name, "PRAGMA table_info(frontier_catalog)", [], |row| row.get(1))
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    if !columns.iter().any(|column| column == "typed_ir") {
        meter.batch(conn, "install", name, "ALTER TABLE frontier_catalog ADD COLUMN typed_ir INTEGER NOT NULL DEFAULT 0")
            .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    }
    let row = meter
        .one(conn, "install", name, &sql, [name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? != 0))
        })
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    Ok(row)
}

/// Previous extension files stored SQL in `frontier_catalog.sql`. Convert
/// every row before renaming the column so the schema change and all values
/// become visible together. Collector tables and frontier counters stay put.
fn migrate_legacy_catalog(conn: &Connection, meter: &mut Meter) -> Result<(), EngineError> {
    let columns: Vec<String> = meter
        .rows(
            conn,
            "install",
            catalog(),
            "PRAGMA table_info(frontier_catalog)",
            [],
            |row| row.get(1),
        )
        .map_err(|e| {
            EngineError::new(Stage::Install, catalog(), ErrorKind::Sqlite(e.to_string()))
        })?;
    if columns.iter().any(|c| c == "program") {
        return Ok(());
    }
    if !columns.iter().any(|c| c == "sql") {
        return Err(EngineError::new(
            Stage::Install,
            catalog(),
            ErrorKind::State("catalog has neither program nor sql column".into()),
        ));
    }
    let rows: Vec<(String, String)> = meter
        .rows(
            conn,
            "install",
            catalog(),
            "SELECT name, sql FROM frontier_catalog ORDER BY name",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| {
            EngineError::new(Stage::Install, catalog(), ErrorKind::Sqlite(e.to_string()))
        })?;
    let converted: Vec<(String, String)> = rows
        .into_iter()
        .map(|(name, sql)| {
            let compiled = compile(conn, &name, &sql)?;
            let ir = plan::lower_ir(&compiled, &|table| source_types(conn, table))?;
            let json = serde_json::to_string(&ir).map_err(|e| {
                EngineError::new(Stage::Install, &name, ErrorKind::State(e.to_string()))
            })?;
            Ok((name, json))
        })
        .collect::<Result<_, EngineError>>()?;

    let mut convert = || -> Result<(), EngineError> {
        meter.batch(conn, "install", catalog(),
            "SAVEPOINT frontier_sp_catalog_migrate; PRAGMA writable_schema=ON; \
             ALTER TABLE frontier_catalog RENAME COLUMN sql TO program; PRAGMA writable_schema=OFF;"
        ).map_err(|e| EngineError::new(Stage::Install, catalog(), ErrorKind::Sqlite(e.to_string())))?;
        for (name, json) in &converted {
            meter
                .exec(
                    conn,
                    "install",
                    catalog(),
                    "UPDATE frontier_catalog SET program = ?1 WHERE name = ?2",
                    (json, name),
                )
                .map_err(|e| {
                    EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string()))
                })?;
        }
        meter
            .batch(
                conn,
                "install",
                catalog(),
                "RELEASE frontier_sp_catalog_migrate;",
            )
            .map_err(|e| {
                EngineError::new(Stage::Install, catalog(), ErrorKind::Sqlite(e.to_string()))
            })?;
        Ok(())
    };
    if let Err(error) = convert() {
        let _ = conn.execute_batch("PRAGMA writable_schema=OFF;");
        rollback(conn, "frontier_sp_catalog_migrate");
        return Err(error);
    }
    Ok(())
}

fn next_install(conn: &Connection, meter: &mut Meter) -> Result<u64, EngineError> {
    let sql = format!(
        "SELECT COALESCE(MAX(install), 0) + 1 FROM {}",
        quote(catalog())
    );
    meter
        .one(conn, "install", catalog(), &sql, [], |row| {
            row.get::<_, i64>(0).map(|v| v as u64)
        })
        .map_err(|e| EngineError::new(Stage::Install, catalog(), ErrorKind::Sqlite(e.to_string())))?
        .ok_or_else(|| {
            EngineError::new(
                Stage::Install,
                catalog(),
                ErrorKind::Sqlite("install sequence read failed".into()),
            )
        })
}

fn build_installed(name: &str, install: u64, mut compiled: Compiled, ir: IrProgram, sql_text: bool) -> Installed {
    for (i, scan) in compiled.scans.iter_mut().enumerate() {
        scan.stage = scan_stage(name, i);
    }
    for (i, join) in compiled.joins.iter_mut().enumerate() {
        join.delta = join_delta(name, i);
    }
    for (i, anti) in compiled.antis.iter_mut().enumerate() {
        anti.delta = anti_delta(name, i);
    }
    for (i, topk) in compiled.topks.iter_mut().enumerate() {
        topk.delta = topk_delta(name, i);
    }
    let sqls = build_settle_sql(name, &compiled);
    Installed {
        name: name.to_string(),
        ir,
        sql_text,
        install,
        output: compiled.output.clone(),
        sources: compiled.sources.clone(),
        derived: Vec::new(),
        plan: compiled,
        sqls,
    }
}

fn push_savepoint(sql: &mut String, sp: &str) {
    sql.push_str(&format!("SAVEPOINT {sp};"));
}

fn release(conn: &Connection, sp: &str) {
    let _ = conn.execute_batch(&format!("RELEASE {sp};"));
}

fn rollback(conn: &Connection, sp: &str) {
    let _ = conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp};"));
}

fn push_catalog_ddl(sql: &mut String) {
    sql.push_str(&format!(
        "CREATE TABLE IF NOT EXISTS {c}(name TEXT PRIMARY KEY, program TEXT NOT NULL, frontier INTEGER NOT NULL DEFAULT 0, install INTEGER NOT NULL);\
         CREATE TABLE IF NOT EXISTS {cc}(name TEXT NOT NULL, pos INTEGER NOT NULL, col TEXT NOT NULL, PRIMARY KEY(name, pos));",
        c = quote(catalog()),
        cc = quote(catalog_column()),
    ));
}

fn push_program_ddl(inst: &Installed, sql: &mut String) {
    let p = &inst.name;
    let plan = &inst.plan;
    let width = plan.stage_width.max(1);
    let stage_cols: Vec<String> = (0..width).map(|i| format!("v{i}")).collect();

    // The transaction staging table.
    sql.push_str(&format!(
        "CREATE TABLE IF NOT EXISTS {t}(__seq INTEGER NOT NULL, __table TEXT NOT NULL, __sign INTEGER NOT NULL, {v});",
        t = quote(stage(p)),
        v = stage_cols.join(","),
    ));
    if let Root::Nodes(nodes) = &plan.root {
        // Each source delta is a CTE over its own stage rows.
        sql.push_str(&format!(
            "CREATE INDEX IF NOT EXISTS {} ON {}(__table);",
            quote(format!("{}_table", stage(p))),
            quote(stage(p)),
        ));
        for ddl in &nodes.ddl {
            sql.push_str(ddl);
            sql.push(';');
        }
        let names = plan
            .output
            .iter()
            .map(|c| quote(&c.name))
            .collect::<Vec<_>>();
        sql.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {d}(__sign INTEGER NOT NULL,{cols});",
            d = quote(delta(p)),
            cols = names.join(",")
        ));
        let types = &inst.ir.rel(inst.ir.outputs[0]).expect("output relation").cols;
        let select = names
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{} AS {name}", decode_sql(types[i], &format!("c{i}"))))
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(
            "CREATE VIEW IF NOT EXISTS {v} AS SELECT {select} FROM ({snapshot});",
            v = quote(view(p)),
            snapshot = nodes.output_snapshot
        ));
        let support = names.iter().enumerate().map(|(i, name)| {
            format!("{} AS {name}", decode_sql(types[i], &format!("c{i}")))
        }).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(
            "CREATE VIEW IF NOT EXISTS {v} AS SELECT {support},w AS __weight FROM ({snapshot});",
            v = quote(root(p)), snapshot = nodes.output_snapshot
        ));
        return;
    }

}

fn build_settle_sql(p: &str, plan: &Compiled) -> SettleSql {
    if let Root::Nodes(_) = &plan.root {
        let width = plan.stage_width.max(1);
        let vals = (0..width)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = (0..width)
            .map(|i| format!("?{}", i + 4))
            .collect::<Vec<_>>()
            .join(",");
        let names = plan
            .output
            .iter()
            .map(|c| quote(&c.name))
            .collect::<Vec<_>>()
            .join(",");
        return SettleSql {
            clears: [stage(p), delta(p)]
                .into_iter()
                .map(|o| {
                    let sql = format!("DELETE FROM {};", quote(&o));
                    (o, sql)
                })
                .collect(),
            stage_insert: format!(
                "INSERT INTO {}(__seq,__table,__sign,{vals}) VALUES (?1,?2,?3,{placeholders});",
                quote(stage(p))
            ),
            bump: format!(
                "UPDATE {} SET frontier=frontier+1 WHERE name=?1;",
                quote(catalog())
            ),
            read_frontier: format!("SELECT frontier FROM {} WHERE name=?1;", quote(catalog())),
            read_delta: format!(
                "SELECT __sign,{names} FROM {} ORDER BY {names},__sign;",
                quote(delta(p))
            ),
            weight_delta: Some(format!(
                "SELECT {names},__sign FROM {} ORDER BY {names};",
                quote(delta(p))
            )),
            snapshot: format!("SELECT {names} FROM {} ORDER BY {names};", quote(view(p))),
        };
    }
    unreachable!("runtime plans always use NodesPlan")
}

// ---------------------------------------------------------------------------
// Open and teardown

pub(crate) fn open(conn: &Connection, name: &str) -> Result<Arc<Installed>, EngineError> {
    validate_program_name(name)?;
    let mut meter = Meter::default();
    let Some((json, install, typed_ir)) = catalog_row(conn, name, &mut meter)? else {
        return Err(EngineError::new(
            Stage::Install,
            name,
            ErrorKind::State("no program with this name is installed".into()),
        ));
    };
    let program: IrProgram = serde_json::from_str(&json).map_err(|e| {
        EngineError::new(
            Stage::Install,
            name,
            ErrorKind::State(format!("stored program: {e}")),
        )
    })?;
    let output = meter
        .rows(
            conn,
            "install",
            name,
            "SELECT col FROM frontier_catalog_column WHERE name = ?1 ORDER BY pos",
            [name],
            |row| row.get::<_, String>(0).map(|name| OutputColumn { name }),
        )
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    let compiled = compile_ir(conn, name, &program, output, typed_ir, &HashMap::new())?;
    let mut installed = build_installed(name, install, compiled, program, !typed_ir);
    installed.derived = scan_derived_sources(conn, &installed)?;
    Ok(Arc::new(installed))
}

pub(crate) fn teardown(conn: &Connection, inst: &Installed) -> Result<(), EngineError> {
    let p = &inst.name;
    let span = tracing::info_span!(target: observe::TARGET, observe::TEARDOWN_SPAN, program = p);
    let _guard = span.enter();
    let mut meter = Meter::default();
    let collector_name = collector(p, inst.install);
    let mut sql = String::from("SAVEPOINT frontier_sp_drop;");
    for table in &inst.plan.sources {
        for event in ["ins", "upd", "del"] {
            sql.push_str(&format!(
                "DROP TRIGGER IF EXISTS {t};",
                t = quote(&format!("{collector_name}_{table}_{event}")),
            ));
        }
    }
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&collector_name)));
    sql.push_str(&format!("DROP VIEW IF EXISTS {};", quote(view(p))));
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(delta(p))));
    if let Root::Nodes(nodes) = &inst.plan.root {
        sql.push_str(&format!("DROP VIEW IF EXISTS {};", quote(root(p))));
        for (kind, object) in nodes.objects.iter().rev() {
            sql.push_str(&format!("DROP {kind} IF EXISTS {};", quote(object)));
        }
    }
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(touch(p))));
    for scan in &inst.plan.scans {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&scan.stage)));
    }
    for join in &inst.plan.joins {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&join.delta)));
    }
    for anti in &inst.plan.antis {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&anti.delta)));
    }
    for topk in &inst.plan.topks {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&topk.delta)));
    }
    if !matches!(inst.plan.root, Root::Nodes(_)) {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(root(p))));
    }
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(stage(p))));
    // Source indexes: every distinct (table, columns) pair this program made.
    let mut indexed: Vec<(String, Vec<String>)> = Vec::new();
    for join in &inst.plan.joins {
        let left = &inst.plan.scans[join.left];
        let right = &inst.plan.scans[join.right];
        for (table, cols) in [
            (&left.table, &join.left_key),
            (&right.table, &join.right_key),
        ] {
            if inst.derived.iter().any(|d| d == table) {
                continue;
            }
            if !indexed.iter().any(|(t, c)| t == table && c == cols) {
                indexed.push((table.clone(), cols.clone()));
            }
        }
    }
    for anti in &inst.plan.antis {
        let left = &inst.plan.scans[anti.left];
        let right = &inst.plan.scans[anti.right];
        for (table, cols) in [
            (&left.table, &anti.left_key),
            (&right.table, &anti.right_key),
        ] {
            if cols.is_empty() || inst.derived.iter().any(|d| d == table) {
                continue;
            }
            if !indexed.iter().any(|(t, c)| t == table && c == cols) {
                indexed.push((table.clone(), cols.clone()));
            }
        }
    }
    for topk in &inst.plan.topks {
        if topk.key.is_empty() {
            continue;
        }
        let table = &inst.plan.scans[topk.scan].table;
        if !inst.derived.iter().any(|d| d == table)
            && !indexed.iter().any(|(t, c)| t == table && c == &topk.key)
        {
            indexed.push((table.clone(), topk.key.clone()));
        }
    }
    if let Root::Group {
        scan,
        keys,
        extremes,
        ..
    } = &inst.plan.root
    {
        let source = &inst.plan.scans[*scan];
        for extreme in extremes {
            let mut cols = keys
                .iter()
                .map(|k| source.needed[k.take].clone())
                .collect::<Vec<_>>();
            cols.push(source.needed[extreme.take].clone());
            if !inst.derived.iter().any(|d| d == &source.table)
                && !indexed
                    .iter()
                    .any(|(t, c)| t == &source.table && c == &cols)
            {
                indexed.push((source.table.clone(), cols));
            }
        }
    }
    for (i, _) in indexed.iter().enumerate() {
        sql.push_str(&format!(
            "DROP INDEX IF EXISTS {};",
            quote(&source_index(p, i))
        ));
    }
    sql.push_str(&format!(
        "DELETE FROM {c} WHERE name = '{p}';DELETE FROM {cc} WHERE name = '{p}';RELEASE frontier_sp_drop;",
        c = quote(catalog()),
        cc = quote(catalog_column()),
        p = p.replace('\'', "''"),
    ));
    meter
        .batch(conn, "teardown", p, &sql)
        .map_err(|e| EngineError::new(Stage::Teardown, p, ErrorKind::Sqlite(e.to_string())))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Catalog row helpers used by the settle path
pub(crate) fn cell_of(value: &Value) -> crate::Cell {
    match value {
        Value::Null => crate::Cell::Null,
        Value::Integer(v) => crate::Cell::Integer(*v),
        Value::Real(v) => crate::Cell::Real(*v),
        Value::Text(v) => crate::Cell::Text(v.clone()),
        Value::Blob(v) => crate::Cell::Blob(v.clone()),
    }
}
