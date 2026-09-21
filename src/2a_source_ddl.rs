//! Source DDL coordinated with SQLite's own ALTER TABLE query rewriting.
use crate::{
    catalog::{self, error, quote},
    census::{self, Phase},
};
use rusqlite::{functions::FunctionFlags, Connection, Result};
use sqlite3_parser::{
    ast::{Cmd, Stmt},
    lexer::sql::Parser,
    Bump, FallibleIterator,
};

/// Names the catalog probes on this path; none touch a source row.
const CATALOG_OBJECT: &str = "catalog";

fn dependents(db: &Connection, source: &str) -> Result<Vec<(String, String)>> {
    let exists: bool = census::query(
        db,
        Phase::Declare,
        CATALOG_OBJECT,
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_sources')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(vec![]);
    }
    census::query_map(
        db,
        Phase::Declare,
        source,
        "SELECT DISTINCT v.name,v.query_sql FROM main.__ivm_views v JOIN main.__ivm_sources s ON s.view_name=v.name WHERE s.table_name=?1 ORDER BY v.id",
        [source],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}
fn atomic<T>(db: &Connection, object: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    census::batch(db, Phase::Declare, object, "SAVEPOINT __ivm_source_ddl")?;
    match f() {
        Ok(value) => {
            census::batch(db, Phase::Declare, object, "RELEASE __ivm_source_ddl")?;
            Ok(value)
        }
        Err(e) => {
            census::batch(
                db,
                Phase::Declare,
                object,
                "ROLLBACK TO __ivm_source_ddl; RELEASE __ivm_source_ddl",
            )?;
            Err(e)
        }
    }
}
fn source(db: &Connection, name: &str) -> Result<String> {
    if name.to_ascii_lowercase().starts_with("__ivm_") {
        return Err(error("reserved source name"));
    }
    census::query(db,Phase::Declare,name,"SELECT name FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='table' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%'",[name],|r|r.get(0)).map_err(|_|error("ordinary main source table required"))
}
/// Bumped after every query_sql rewrite. A live table on the same connection
/// sees the rewrite only through this; another process reconnects through xConnect.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

fn rename(db: &Connection, old: &str, new: &str, column: Option<&str>) -> Result<()> {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let old = source(db, old)?;
    if new.to_ascii_lowercase().starts_with("__ivm_") || new.is_empty() {
        return Err(error("invalid destination name"));
    }
    let views = dependents(db, &old)?;
    for (v, _) in &views {
        catalog::manifest(db, v, None)?;
    }
    let legacy: bool = census::query(db, Phase::Declare, CATALOG_OBJECT, "PRAGMA legacy_alter_table", [], |r| r.get(0))?;
    census::pragma(db, Phase::Declare, CATALOG_OBJECT, "legacy_alter_table", false)?;
    let result = atomic(db, new, || {
        for (i, (_, sql)) in views.iter().enumerate() {
            census::batch(
                db,
                Phase::Declare,
                new,
                &format!(
                    "CREATE TEMP VIEW {} AS {sql}",
                    quote(&format!("__ivm_ddl_{i}"))
                ),
            )?;
        }
        census::batch(
            db,
            Phase::Declare,
            new,
            &if let Some(column) = column {
                format!(
                    "ALTER TABLE main.{} RENAME COLUMN {} TO {}",
                    quote(&old),
                    quote(column),
                    quote(new)
                )
            } else {
                format!("ALTER TABLE main.{} RENAME TO {}", quote(&old), quote(new))
            },
        )?;
        for (i, (v, _)) in views.iter().enumerate() {
            let helper = format!("__ivm_ddl_{i}");
            let ddl: String = census::query(
                db,
                Phase::Declare,
                v,
                "SELECT sql FROM temp.sqlite_schema WHERE name=?1",
                [&helper],
                |r| r.get(0),
            )?;
            let arena = Bump::new();
            let mut parser = Parser::new(&arena, ddl.as_bytes());
            let sql = match parser.next().map_err(|e| error(e.to_string()))? {
                Some(Cmd::Stmt(Stmt::CreateView { select, .. })) => {
                    Cmd::Stmt(Stmt::Select(select)).to_string()
                }
                _ => return Err(error("source DDL query rewrite failed")),
            };
            // A renamed column can change payload ordering. Rebind and
            // regenerate the hooks while preserving every state/index B-tree;
            // a legacy row rebuilds its shadows first, against the rewritten SQL.
            let generic: bool = census::query(db,Phase::Declare,v,"SELECT s.generic FROM main.__ivm_schema s JOIN main.__ivm_views v ON v.id=s.id WHERE v.name=?1",[v],|r|r.get(0))?;
            crate::vtab::drop_triggers(db, v)?;
            let plan = crate::relational::bind(db, &sql)?;
            if generic {
                crate::relational_maintenance::hooks(db, v, &plan)?;
            } else {
                crate::vtab::convert(db, v, &plan)?;
            }
            census::exec(
                db,
                Phase::Declare,
                v,
                "UPDATE main.__ivm_views SET query_sql=?1 WHERE name=?2",
                [&sql, v],
            )?;
            if let Some(column) = column {
                census::exec(db,Phase::Declare,v,"UPDATE main.__ivm_columns SET column_name=?1 WHERE view_name=?2 AND column_name=?3 AND source_ordinal IN(SELECT source_ordinal FROM main.__ivm_sources WHERE view_name=?2 AND table_name=?4)",rusqlite::params![new,v,column,old])?;
            } else {
                census::exec(db,Phase::Declare,v,"UPDATE main.__ivm_sources SET table_name=?1 WHERE view_name=?2 AND table_name=?3",[new,v,&old])?;
            }
            census::exec(db,Phase::Declare,v,"UPDATE main.__ivm_objects SET definition=(SELECT sql FROM main.sqlite_schema WHERE type=object_type AND name=object_name) WHERE view_name=?1",[v])?;
            census::batch(db, Phase::Declare, v, &format!("DROP VIEW temp.{}", quote(&helper)))?;
        }
        Ok(())
    });
    census::pragma(db, Phase::Declare, CATALOG_OBJECT, "legacy_alter_table", legacy)?;
    result
}
pub fn register(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
    db.create_scalar_function(c"sqlite_ivm_rename_source", 2, flags, |ctx| {
        let old: String = ctx.get(0)?;
        let new: String = ctx.get(1)?;
        let db = unsafe { ctx.get_connection()? };
        rename(&db, &old, &new, None)?;
        Ok(new)
    })?;
    db.create_scalar_function(c"sqlite_ivm_rename_column", 3, flags, |ctx| {
        let table: String = ctx.get(0)?;
        let old: String = ctx.get(1)?;
        let new: String = ctx.get(2)?;
        let db = unsafe { ctx.get_connection()? };
        rename(&db, &table, &new, Some(&old))?;
        Ok(new)
    })?;
    db.create_scalar_function(c"sqlite_ivm_drop_source", 2, flags, |ctx| {
        let table: String = ctx.get(0)?;
        let cascade: bool = ctx.get(1)?;
        let db = unsafe { ctx.get_connection()? };
        let table = source(&db, &table)?;
        let views = dependents(&db, &table)?;
        if !cascade && !views.is_empty() {
            return Err(error(
                "source has managed dependents; cascade must be explicitly enabled",
            ));
        }
        atomic(&db, &table, || {
            for (v, _) in &views {
                census::batch(&db, Phase::Teardown, v, &format!("DROP TABLE main.{}", quote(v)))?;
            }
            census::batch(&db, Phase::Teardown, &table, &format!("DROP TABLE main.{}", quote(&table)))
        })?;
        Ok(table)
    })
}
