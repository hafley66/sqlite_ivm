//! Storage layout and program lifecycle.
//!
//! Per installed program `P` (names validated identifiers; engine objects are
//! prefixed `frontier_`):
//!
//! | object | shape |
//! | --- | --- |
//! | `frontier_catalog` | `name TEXT PK, sql TEXT, frontier INTEGER, install INTEGER` |
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

use crate::error::{EngineError, ErrorKind, Stage};
use crate::meter::Meter;
use crate::observe;
use crate::plan::{self, Compiled, Root};
use crate::OutputColumn;
use sqlite_ext::rusqlite::{types::Value, Connection};
use std::sync::Arc;

pub(crate) struct Installed {
    pub name: String,
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
    pub scan_fills: Vec<(String, String)>,
    pub join_fills: Vec<(String, String)>,
    pub root_touch: String,
    pub root_upsert: String,
    pub root_delete: String,
    pub root_delta: String,
    pub bump: String,
    pub read_frontier: String,
    pub read_delta: String,
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

pub(crate) fn root_index(p: &str) -> String {
    format!("frontier_{p}_rootk")
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
    let mut derived = Vec::new();
    for source in &inst.sources {
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

pub(crate) fn install(
    conn: &Connection,
    name: &str,
    select_sql: &str,
    watch: Watch,
) -> Result<Arc<Installed>, EngineError> {
    validate_program_name(name)?;
    let _guard =
        tracing::info_span!(target: observe::TARGET, observe::INSTALL_SPAN, program = name)
            .entered();
    let compiled = compile(conn, name, select_sql)?;
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
        let mut built = build_installed(name, install, compiled);
        built.derived = scan_derived_sources(conn, &built)?;
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
    push_catalog_ddl(&mut sql);
    push_program_ddl(&installed, &mut sql);
    // The program's catalog row: SQL text for reopen, frontier counter, and
    // one row per output column instead of any JSON payload.
    sql.push_str(&format!(
        "INSERT INTO {c}(name, sql, frontier, install) VALUES ('{n}', '{s}', 0, {i});",
        c = quote(catalog()),
        n = name.replace('\'', "''"),
        s = select_sql.replace('\'', "''"),
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

fn compile(conn: &Connection, name: &str, select_sql: &str) -> Result<Compiled, EngineError> {
    let columns = |table: &str| -> Option<Vec<String>> {
        let mut fresh = Meter::default();
        table_columns(conn, table, &mut fresh)
            .ok()
            .filter(|cols| !cols.is_empty())
    };
    plan::compile(name, select_sql, &columns)
}

fn catalog_row(
    conn: &Connection,
    name: &str,
    meter: &mut Meter,
) -> Result<Option<(String, u64)>, EngineError> {
    let sql = format!(
        "SELECT sql, install FROM {} WHERE name = ?1",
        quote(catalog())
    );
    // First install on a database has no catalog yet; create it before reads.
    meter
        .batch(conn, "install", name, &format!(
            "CREATE TABLE IF NOT EXISTS {}(name TEXT PRIMARY KEY, sql TEXT NOT NULL, frontier INTEGER NOT NULL DEFAULT 0, install INTEGER NOT NULL);\
             CREATE TABLE IF NOT EXISTS {}(name TEXT NOT NULL, pos INTEGER NOT NULL, col TEXT NOT NULL, PRIMARY KEY(name, pos))",
            quote(catalog()),
            quote(catalog_column()),
        ))
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    let row = meter
        .one(conn, "install", name, &sql, [name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })
        .map_err(|e| EngineError::new(Stage::Install, name, ErrorKind::Sqlite(e.to_string())))?;
    Ok(row)
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

fn build_installed(name: &str, install: u64, mut compiled: Compiled) -> Installed {
    for (i, scan) in compiled.scans.iter_mut().enumerate() {
        scan.stage = scan_stage(name, i);
    }
    for (i, join) in compiled.joins.iter_mut().enumerate() {
        join.delta = join_delta(name, i);
    }
    let sqls = build_settle_sql(name, &compiled);
    Installed {
        name: name.to_string(),
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
        "CREATE TABLE IF NOT EXISTS {c}(name TEXT PRIMARY KEY, sql TEXT NOT NULL, frontier INTEGER NOT NULL DEFAULT 0, install INTEGER NOT NULL);\
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
    // One netted-delta table per scan.
    for scan in &plan.scans {
        let cols: Vec<String> = scan.needed.iter().map(|c| quote(c)).collect();
        sql.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {t}({cols}, __mult INTEGER NOT NULL);",
            t = quote(&scan.stage),
            cols = cols.join(","),
        ));
    }
    // One derivation-delta table per join.
    for join in &plan.joins {
        let cols: Vec<String> = join.out_names.iter().map(|c| quote(c)).collect();
        sql.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {t}({cols}, __mult INTEGER NOT NULL);",
            t = quote(&join.delta),
            cols = cols.join(","),
        ));
    }
    // Root, its key index, the touch table and the delta table.
    let key_cols: Vec<String> = plan.output.iter().map(|c| quote(&c.name)).collect();
    match &plan.root {
        Root::Union { .. } => {
            sql.push_str(&format!(
                "CREATE TABLE IF NOT EXISTS {r}({keys}, __weight INTEGER NOT NULL);\
                 CREATE UNIQUE INDEX IF NOT EXISTS {rk} ON {r}({keys});",
                r = quote(root(p)),
                keys = key_cols.join(","),
                rk = quote(root_index(p)),
            ));
            sql.push_str(&format!(
                "CREATE TABLE IF NOT EXISTS {t}({keys}, __bw INTEGER NOT NULL);",
                t = quote(touch(p)),
                keys = key_cols.join(","),
            ));
        }
        Root::Group { keys, sums, .. } => {
            let key_names: Vec<String> = keys.iter().map(|k| quote(&k.name)).collect();
            let mut state_cols: Vec<String> = key_names.clone();
            state_cols.push("__n INTEGER NOT NULL".into());
            for i in 0..sums.len() {
                state_cols.push(format!("__s{i} INTEGER NOT NULL"));
            }
            sql.push_str(&format!(
                "CREATE TABLE IF NOT EXISTS {r}({cols});\
                 CREATE UNIQUE INDEX IF NOT EXISTS {rk} ON {r}({keys});",
                r = quote(root(p)),
                cols = state_cols.join(","),
                rk = quote(root_index(p)),
                keys = key_names.join(","),
            ));
            let mut touch_cols: Vec<String> = key_names;
            touch_cols.push("__bn INTEGER NOT NULL".into());
            for i in 0..sums.len() {
                touch_cols.push(format!("__bs{i} INTEGER NOT NULL"));
            }
            sql.push_str(&format!(
                "CREATE TABLE IF NOT EXISTS {t}({cols});",
                t = quote(touch(p)),
                cols = touch_cols.join(","),
            ));
        }
    }
    sql.push_str(&format!(
        "CREATE TABLE IF NOT EXISTS {d}(__sign INTEGER NOT NULL, {cols});",
        d = quote(delta(p)),
        cols = key_cols.join(","),
    ));
    // The visible-output view.
    match &plan.root {
        Root::Union { .. } => {
            sql.push_str(&format!(
                "CREATE VIEW IF NOT EXISTS {v} AS SELECT {cols} FROM {r} WHERE __weight > 0;",
                v = quote(view(p)),
                cols = key_cols.join(","),
                r = quote(root(p)),
            ));
        }
        Root::Group { keys, sums, .. } => {
            let mut named: Vec<String> = keys.iter().map(|k| quote(&k.name)).collect();
            named.push(format!("__n AS {}", quote(&inst.count_name())));
            for (i, sum) in sums.iter().enumerate() {
                named.push(format!("__s{i} AS {}", quote(&sum.name)));
            }
            sql.push_str(&format!(
                "CREATE VIEW IF NOT EXISTS {v} AS SELECT {cols} FROM {r} WHERE __n > 0;",
                v = quote(view(p)),
                cols = named.join(","),
                r = quote(root(p)),
            ));
        }
    }
    // Source indexes for every join probe side.
    let mut indexed: Vec<(String, Vec<String>)> = Vec::new();
    for join in &plan.joins {
        let left = &plan.scans[join.left];
        let right = &plan.scans[join.right];
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
    for (i, (table, cols)) in indexed.iter().enumerate() {
        let cols: Vec<String> = cols.iter().map(|c| quote(c)).collect();
        sql.push_str(&format!(
            "CREATE INDEX IF NOT EXISTS {ix} ON {t}({cols});",
            ix = quote(&source_index(p, i)),
            t = quote(table),
            cols = cols.join(","),
        ));
    }
}

impl Installed {
    /// The output alias for the aggregate count column, if any.
    pub(crate) fn count_name(&self) -> String {
        match &self.plan.root {
            Root::Group { keys, .. } => {
                let key_len = keys.len();
                self.plan
                    .output
                    .get(key_len)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "count".into())
            }
            _ => "count".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Settle SQL generation

fn build_settle_sql(p: &str, plan: &Compiled) -> SettleSql {
    let mut clears = Vec::new();
    for object in [stage(p), touch(p), delta(p)] {
        clears.push((object.clone(), format!("DELETE FROM {};", quote(&object))));
    }
    for scan in &plan.scans {
        clears.push((
            scan.stage.clone(),
            format!("DELETE FROM {};", quote(&scan.stage)),
        ));
    }
    for join in &plan.joins {
        clears.push((
            join.delta.clone(),
            format!("DELETE FROM {};", quote(&join.delta)),
        ));
    }

    let width = plan.stage_width.max(1);
    let placeholders: Vec<String> = (0..width).map(|i| format!("?{}", i + 4)).collect();
    let stage_insert = format!(
        "INSERT INTO {t}(__seq, __table, __sign, {v}) VALUES (?1, ?2, ?3, {ph});",
        t = quote(stage(p)),
        v = (0..width)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(","),
        ph = placeholders.join(","),
    );

    let mut scan_fills = Vec::new();
    for scan in &plan.scans {
        let positions: Vec<usize> = scan
            .needed
            .iter()
            .map(|c| scan.columns.iter().position(|x| x == c).unwrap_or(0))
            .collect();
        let select_cols: Vec<String> = positions.iter().map(|i| format!("v{i}")).collect();
        let out_cols: Vec<String> = scan.needed.iter().map(|c| quote(c)).collect();
        let group: Vec<String> = (1..=positions.len()).map(|i| i.to_string()).collect();
        scan_fills.push((
            scan.table.clone(),
            format!(
                "INSERT INTO {into}({cols}, __mult) SELECT {sel}, SUM(__sign) FROM {st} WHERE __table = ?1 GROUP BY {g} HAVING SUM(__sign) <> 0;",
                into = quote(&scan.stage),
                cols = out_cols.join(","),
                sel = select_cols.join(","),
                st = quote(stage(p)),
                g = group.join(","),
            ),
        ));
    }

    let mut join_fills = Vec::new();
    for join in &plan.joins {
        let left = &plan.scans[join.left];
        let right = &plan.scans[join.right];
        let l_names: Vec<&String> = join.left_proj.iter().map(|i| &left.needed[*i]).collect();
        let r_names: Vec<&String> = join.right_proj.iter().map(|i| &right.needed[*i]).collect();
        let out_names = &join.out_names;
        let on: Vec<String> = join
            .left_key
            .iter()
            .zip(&join.right_key)
            .map(|(lk, rk)| format!("r.{} = l.{}", quote(rk), quote(lk)))
            .collect();
        let on = on.join(" AND ");
        let select = |l_expr: &str, r_expr: &str, w: &str| -> String {
            let mut exprs: Vec<String> = Vec::new();
            for (i, name) in l_names.iter().enumerate() {
                let alias = &out_names[i];
                exprs.push(format!("{l_expr}.{} AS {}", quote(name), quote(alias)));
            }
            for (i, name) in r_names.iter().enumerate() {
                let alias = &out_names[l_names.len() + i];
                exprs.push(format!("{r_expr}.{} AS {}", quote(name), quote(alias)));
            }
            exprs.push(format!("{w} AS w"));
            exprs.join(", ")
        };
        let group: Vec<String> = (1..=out_names.len()).map(|i| i.to_string()).collect();
        let out_cols: Vec<String> = out_names.iter().map(|c| quote(c)).collect();
        join_fills.push((
            join.delta.clone(),
            format!(
                "INSERT INTO {into}({cols}, __mult) SELECT {outc}, SUM(w) FROM (\
                   SELECT {t1} FROM {ls} l JOIN {rt} r ON {on}\
                   UNION ALL SELECT {t2} FROM {lt} l JOIN {rs} r ON {on}\
                   UNION ALL SELECT {t3} FROM {ls} l JOIN {rs} r ON {on}\
                 ) GROUP BY {g};",
                into = quote(&join.delta),
                cols = out_cols.join(","),
                outc = out_cols.join(","),
                t1 = select("l", "r", "l.__mult"),
                t2 = select("l", "r", "r.__mult"),
                t3 = select("l", "r", "-(l.__mult * r.__mult)"),
                ls = quote(&left.stage),
                rs = quote(&right.stage),
                lt = quote(&left.table),
                rt = quote(&right.table),
                g = group.join(","),
            ),
        ));
    }

    let arity = plan.output.len();
    let out_cols: Vec<String> = plan.output.iter().map(|c| quote(&c.name)).collect();
    let group_ord: Vec<String> = (1..=arity).map(|i| i.to_string()).collect();

    // Branch deltas: one SELECT per branch, UNION ALL-ed.
    let branch_sql = |plan: &Compiled| -> String {
        let mut parts = Vec::new();
        if let Root::Union { branches } = &plan.root {
            for branch in branches {
                match branch {
                    plan::BranchRef::Scan { scan, takes } => {
                        let scan = &plan.scans[*scan];
                        let cols: Vec<String> =
                            takes.iter().map(|i| quote(&scan.needed[*i])).collect();
                        parts.push(format!(
                            "SELECT {cols}, __mult FROM {t}",
                            cols = cols.join(","),
                            t = quote(&scan.stage),
                        ));
                    }
                    plan::BranchRef::Join(j) => {
                        let join = &plan.joins[*j];
                        let cols: Vec<String> = join.out_names.iter().map(|c| quote(c)).collect();
                        parts.push(format!(
                            "SELECT {cols}, __mult FROM {t}",
                            cols = cols.join(","),
                            t = quote(&join.delta),
                        ));
                    }
                }
            }
        }
        parts.join(" UNION ALL ")
    };

    let (root_touch, root_upsert, root_delete, root_delta, snapshot) = match &plan.root {
        Root::Union { .. } => {
            let d = branch_sql(plan);
            let key_cond: Vec<String> = plan
                .output
                .iter()
                .map(|c| format!("r.{} = d.{}", quote(&c.name), quote(&c.name)))
                .collect();
            let key_cond = key_cond.join(" AND ");
            let touch_sql = format!(
                "INSERT INTO {t}({cols}, __bw) SELECT {dc}, COALESCE(r.__weight, 0) FROM (SELECT {cols} FROM ({d}) GROUP BY {g}) d LEFT JOIN {r} r ON {cond};",
                t = quote(touch(p)),
                cols = out_cols.join(","),
                dc = out_cols
                    .iter()
                    .map(|c| format!("d.{c}"))
                    .collect::<Vec<_>>()
                    .join(","),
                d = d,
                g = group_ord.join(","),
                r = quote(root(p)),
                cond = key_cond,
            );
            let upsert_sql = format!(
                "INSERT INTO {r}({cols}, __weight) SELECT {cols}, SUM(__mult) FROM ({d}) GROUP BY {g} ON CONFLICT({cols}) DO UPDATE SET __weight = __weight + excluded.__weight;",
                r = quote(root(p)),
                cols = out_cols.join(","),
                d = d,
                g = group_ord.join(","),
            );
            let delete_sql = format!(
                "DELETE FROM {r} WHERE __weight <= 0 AND ({cols}) IN (SELECT {cols} FROM {t});",
                r = quote(root(p)),
                cols = out_cols.join(","),
                t = quote(touch(p)),
            );
            let delta_sql = format!(
                "INSERT INTO {dl}(__sign, {cols}) SELECT CASE WHEN t.__bw > 0 THEN -1 ELSE 1 END, {tc} FROM {t} t LEFT JOIN {r} r ON {tcond} WHERE (t.__bw > 0) != (COALESCE(r.__weight, 0) > 0);",
                dl = quote(delta(p)),
                cols = out_cols.join(","),
                tc = out_cols
                    .iter()
                    .map(|c| format!("t.{c}"))
                    .collect::<Vec<_>>()
                    .join(","),
                t = quote(touch(p)),
                r = quote(root(p)),
                tcond = key_cond.replace("d.", "t."),
            );
            let snapshot_sql = format!(
                "SELECT {cols} FROM {r} WHERE __weight > 0 ORDER BY {cols};",
                cols = out_cols.join(","),
                r = quote(root(p)),
            );
            (touch_sql, upsert_sql, delete_sql, delta_sql, snapshot_sql)
        }
        Root::Group { scan, keys, sums } => {
            let group_scan = &plan.scans[*scan];
            let key_names: Vec<String> = keys.iter().map(|k| quote(&k.name)).collect();
            let key_takes: Vec<String> = keys
                .iter()
                .map(|k| quote(&group_scan.needed[k.take]))
                .collect();
            let sum_exprs: Vec<String> = sums
                .iter()
                .map(|s| format!("SUM(__mult * {})", quote(&group_scan.needed[s.take])))
                .collect();
            let g = &group_ord[0];
            let key_cond: Vec<String> = keys
                .iter()
                .map(|k| format!("r.{} = d.{}", quote(&k.name), quote(&k.name)))
                .collect();
            let key_cond = key_cond.join(" AND ");
            let touch_sql = format!(
                "INSERT INTO {t}({keys}, __bn, {bs}) SELECT {dc}, COALESCE(r.__n, 0), {bc} FROM (SELECT {kt}, SUM(__mult) AS __mn, {se} FROM {st} GROUP BY {g}) d LEFT JOIN {r} r ON {cond};",
                t = quote(touch(p)),
                keys = key_names.join(","),
                bs = (0..sums.len()).map(|i| format!("__bs{i}")).collect::<Vec<_>>().join(","),
                dc = key_names
                    .iter()
                    .map(|c| format!("d.{c}"))
                    .collect::<Vec<_>>()
                    .join(","),
                bc = (0..sums.len())
                    .map(|i| format!("COALESCE(r.__s{i}, 0)"))
                    .collect::<Vec<_>>()
                    .join(","),
                kt = key_takes.join(","),
                se = sum_exprs
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!("{e} AS __ms{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                st = quote(&group_scan.stage),
                r = quote(root(p)),
                cond = key_cond,
                g = g,
            );
            let upsert_sql = format!(
                "INSERT INTO {r}({keys}, __n, {ss}) SELECT {kt}, SUM(__mult), {se} FROM {st} GROUP BY {g} ON CONFLICT({keys}) DO UPDATE SET __n = __n + excluded.__n{extra};",
                r = quote(root(p)),
                keys = key_names.join(","),
                ss = (0..sums.len()).map(|i| format!("__s{i}")).collect::<Vec<_>>().join(","),
                kt = key_takes.join(","),
                se = sum_exprs.join(","),
                st = quote(&group_scan.stage),
                g = g,
                extra = sums
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!(
                        ", __s{i} = __s{i} + excluded.__s{i}"
                    ))
                    .collect::<String>(),
            );
            let delete_sql = format!(
                "DELETE FROM {r} WHERE __n <= 0 AND ({keys}) IN (SELECT {keys} FROM {t});",
                r = quote(root(p)),
                keys = key_names.join(","),
                t = quote(touch(p)),
            );
            let changed = {
                let mut terms = vec!["r.__n != t.__bn".to_string()];
                for i in 0..sums.len() {
                    terms.push(format!("r.__s{i} != t.__bs{i}"));
                }
                terms.join(" OR ")
            };
            let delta_sql = format!(
                "INSERT INTO {dl}(__sign, {cols}) \
                 SELECT -1, {tkeys}, t.__bn, {tbs} FROM {t} t LEFT JOIN {r} r ON {tcond} WHERE t.__bn > 0 AND (COALESCE(r.__n, 0) <= 0 OR {changed}) \
                 UNION ALL \
                 SELECT 1, {rkeys}, r.__n, {rbs} FROM {r} r JOIN {t} t ON {cond2} WHERE r.__n > 0 AND (t.__bn <= 0 OR {changed});",
                dl = quote(delta(p)),
                cols = out_cols.join(","),
                tkeys = key_names
                    .iter()
                    .map(|c| format!("t.{c}"))
                    .collect::<Vec<_>>()
                    .join(","),
                tbs = (0..sums.len())
                    .map(|i| format!("t.__bs{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                t = quote(touch(p)),
                r = quote(root(p)),
                tcond = key_cond.replace("d.", "t."),
                changed = changed,
                rkeys = key_names
                    .iter()
                    .map(|c| format!("r.{c}"))
                    .collect::<Vec<_>>()
                    .join(","),
                rbs = (0..sums.len())
                    .map(|i| format!("r.__s{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                cond2 = key_cond.replace("d.", "t."),
            );
            let snapshot_sql = format!(
                "SELECT {keys}, __n, {ss} FROM {r} WHERE __n > 0 ORDER BY {keys};",
                keys = key_names.join(","),
                ss = (0..sums.len())
                    .map(|i| format!("__s{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                r = quote(root(p)),
            );
            (touch_sql, upsert_sql, delete_sql, delta_sql, snapshot_sql)
        }
    };

    SettleSql {
        clears,
        stage_insert,
        scan_fills,
        join_fills,
        root_touch,
        root_upsert,
        root_delete,
        root_delta,
        bump: format!(
            "UPDATE {c} SET frontier = frontier + 1 WHERE name = ?1;",
            c = quote(catalog())
        ),
        read_frontier: format!(
            "SELECT frontier FROM {c} WHERE name = ?1;",
            c = quote(catalog())
        ),
        read_delta: format!(
            "SELECT __sign, {cols} FROM {d} ORDER BY {cols}, __sign;",
            d = quote(delta(p)),
            cols = out_cols.join(","),
        ),
        snapshot,
    }
}

// ---------------------------------------------------------------------------
// Open and teardown

pub(crate) fn open(conn: &Connection, name: &str) -> Result<Arc<Installed>, EngineError> {
    validate_program_name(name)?;
    let mut meter = Meter::default();
    let Some((sql, install)) = catalog_row(conn, name, &mut meter)? else {
        return Err(EngineError::new(
            Stage::Install,
            name,
            ErrorKind::State("no program with this name is installed".into()),
        ));
    };
    let compiled = compile(conn, name, &sql)?;
    let mut installed = build_installed(name, install, compiled);
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
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(touch(p))));
    for scan in &inst.plan.scans {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&scan.stage)));
    }
    for join in &inst.plan.joins {
        sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(&join.delta)));
    }
    sql.push_str(&format!("DROP TABLE IF EXISTS {};", quote(root(p))));
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
