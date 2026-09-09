//! Source DDL coordinated with SQLite's own ALTER TABLE query rewriting.
use crate::{
    catalog,
    query::{error, quote},
};
use rusqlite::{functions::FunctionFlags, Connection, Result};
use sqlite3_parser::{
    ast::{Cmd, Stmt},
    lexer::sql::Parser,
    Bump, FallibleIterator,
};

fn dependents(db: &Connection, source: &str) -> Result<Vec<(String, String)>> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_sources')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(vec![]);
    }
    db.prepare("SELECT DISTINCT v.name,v.query_sql FROM main.__ivm_views v JOIN main.__ivm_sources s ON s.view_name=v.name WHERE s.table_name=?1 ORDER BY v.id")?
        .query_map([source],|r|Ok((r.get(0)?,r.get(1)?)))?.collect()
}
fn atomic<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    db.execute_batch("SAVEPOINT __ivm_source_ddl")?;
    match f() {
        Ok(value) => {
            db.execute_batch("RELEASE __ivm_source_ddl")?;
            Ok(value)
        }
        Err(e) => {
            db.execute_batch("ROLLBACK TO __ivm_source_ddl; RELEASE __ivm_source_ddl")?;
            Err(e)
        }
    }
}
fn source(db: &Connection, name: &str) -> Result<String> {
    if name.to_ascii_lowercase().starts_with("__ivm_") {
        return Err(error("reserved source name"));
    }
    db.query_row("SELECT name FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='table' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%'",[name],|r|r.get(0)).map_err(|_|error("ordinary main source table required"))
}
fn rename(db: &Connection, old: &str, new: &str, column: Option<&str>) -> Result<()> {
    let old = source(db, old)?;
    if new.to_ascii_lowercase().starts_with("__ivm_") || new.is_empty() {
        return Err(error("invalid destination name"));
    }
    let views = dependents(db, &old)?;
    for (v, _) in &views {
        catalog::manifest(db, v, None)?;
    }
    let legacy: bool = db.query_row("PRAGMA legacy_alter_table", [], |r| r.get(0))?;
    db.pragma_update(None, "legacy_alter_table", false)?;
    let result = atomic(db, || {
        for (i, (_, sql)) in views.iter().enumerate() {
            db.execute_batch(&format!(
                "CREATE TEMP VIEW {} AS {sql}",
                quote(&format!("__ivm_ddl_{i}"))
            ))?;
        }
        db.execute_batch(&if let Some(column) = column {
            format!(
                "ALTER TABLE main.{} RENAME COLUMN {} TO {}",
                quote(&old),
                quote(column),
                quote(new)
            )
        } else {
            format!("ALTER TABLE main.{} RENAME TO {}", quote(&old), quote(new))
        })?;
        for (i, (v, _)) in views.iter().enumerate() {
            let helper = format!("__ivm_ddl_{i}");
            let ddl: String = db.query_row(
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
            // A renamed narrow-path column can change payload ordering. Rebind
            // and regenerate the hooks while preserving every state/index B-tree.
            let generic:bool=db.query_row("SELECT generic FROM main.__ivm_schema s JOIN main.__ivm_views v ON v.id=s.id WHERE v.name=?1",[v],|r|r.get(0))?;
            let triggers=db.prepare("SELECT object_name FROM main.__ivm_objects WHERE view_name=?1 AND object_type='trigger'")?.query_map([v],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?;
            for trigger in triggers {
                db.execute_batch(&format!("DROP TRIGGER main.{}", quote(&trigger)))?;
            }
            if generic {
                crate::relational_maintenance::hooks(db, v, &crate::relational::bind(db, &sql)?)?;
            } else {
                crate::maintenance::create_hooks(db, v, &crate::query::bind(db, &sql)?, false)?;
            }
            db.execute(
                "UPDATE main.__ivm_views SET query_sql=?1 WHERE name=?2",
                [&sql, v],
            )?;
            if let Some(column) = column {
                db.execute("UPDATE main.__ivm_columns SET column_name=?1 WHERE view_name=?2 AND column_name=?3 AND source_ordinal IN(SELECT source_ordinal FROM main.__ivm_sources WHERE view_name=?2 AND table_name=?4)",rusqlite::params![new,v,column,old])?;
            } else {
                db.execute("UPDATE main.__ivm_sources SET table_name=?1 WHERE view_name=?2 AND table_name=?3",[new,v,&old])?;
            }
            db.execute("UPDATE main.__ivm_objects SET definition=(SELECT sql FROM main.sqlite_schema WHERE type=object_type AND name=object_name) WHERE view_name=?1",[v])?;
            db.execute_batch(&format!("DROP VIEW temp.{}", quote(&helper)))?;
        }
        Ok(())
    });
    db.pragma_update(None, "legacy_alter_table", legacy)?;
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
        atomic(&db, || {
            for (v, _) in &views {
                db.execute_batch(&format!("DROP TABLE main.{}", quote(v)))?;
            }
            db.execute_batch(&format!("DROP TABLE main.{}", quote(&table)))
        })?;
        Ok(table)
    })
}
