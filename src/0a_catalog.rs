use crate::census::{self, Phase};
use rusqlite::{Connection, Error, OptionalExtension, Result};

pub fn error(message: impl Into<String>) -> Error {
    Error::UserFunctionError(Box::new(std::io::Error::other(message.into())))
}

pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub source: usize,
    pub name: String,
}

pub fn record_objects(
    db: &Connection,
    name: &str,
    sql: &str,
    tables: &[String],
    columns: &[&Column],
    objects: &[(&str, String)],
) -> Result<()> {
    census::batch(db, Phase::Declare, name, "
        CREATE TABLE IF NOT EXISTS main.__ivm_views(
            id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, query_sql TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS main.__ivm_sources(
            view_name TEXT NOT NULL COLLATE NOCASE, source_ordinal INTEGER NOT NULL,
            table_name TEXT NOT NULL COLLATE NOCASE,
            PRIMARY KEY(view_name,source_ordinal));
        CREATE TABLE IF NOT EXISTS main.__ivm_columns(
            view_name TEXT NOT NULL COLLATE NOCASE, source_ordinal INTEGER NOT NULL,
            column_name TEXT NOT NULL COLLATE NOCASE,
            PRIMARY KEY(view_name,source_ordinal,column_name));
        CREATE TABLE IF NOT EXISTS main.__ivm_objects(
            view_name TEXT NOT NULL COLLATE NOCASE,
            object_type TEXT NOT NULL CHECK(object_type IN ('view','table','trigger','index')),
            object_name TEXT NOT NULL COLLATE NOCASE, definition TEXT NOT NULL,
            PRIMARY KEY(object_type,object_name));
        CREATE INDEX IF NOT EXISTS main.__ivm_objects_by_view ON __ivm_objects(view_name);
        CREATE INDEX IF NOT EXISTS main.__ivm_sources_by_table ON __ivm_sources(table_name);
    ")?;
    census::exec(
        db,
        Phase::Declare,
        name,
        "INSERT INTO main.__ivm_views(name,query_sql) VALUES(?1,?2)",
        [name, sql],
    )?;
    for (source, table) in tables.iter().enumerate() {
        census::exec(
            db,
            Phase::Declare,
            name,
            "INSERT INTO main.__ivm_sources VALUES(?1,?2,?3)",
            rusqlite::params![name, source as i64, table],
        )?;
    }
    for column in columns {
        census::exec(
            db,
            Phase::Declare,
            name,
            "INSERT INTO main.__ivm_columns VALUES(?1,?2,?3)",
            rusqlite::params![name, column.source as i64, column.name],
        )?;
    }
    for (kind, object) in objects {
        let definition: String = census::query(
            db,
            Phase::Declare,
            name,
            "SELECT sql FROM main.sqlite_schema WHERE type=?1 AND name=?2 COLLATE NOCASE",
            [kind, object.as_str()],
            |r| r.get(0),
        )?;
        census::exec(
            db,
            Phase::Declare,
            name,
            "INSERT INTO main.__ivm_objects VALUES(?1,?2,?3,?4)",
            [name, kind, object, &definition],
        )?;
    }
    Ok(())
}

pub fn manifest(
    db: &Connection,
    name: &str,
    rename: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let objects = census::query_map(
        db,
        Phase::Declare,
        name,
        "SELECT object_type,object_name,definition FROM main.__ivm_objects
        WHERE view_name=?1 ORDER BY CASE object_type WHEN 'trigger' THEN 0 WHEN 'index' THEN 1 ELSE 2 END,object_name",
        [name],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
    )?;
    if objects.is_empty() {
        return Err(error("managed view has no object manifest"));
    }
    for (kind, object, definition) in &objects {
        let actual: Option<String> = census::query(
            db,
            Phase::Declare,
            name,
            "SELECT sql FROM main.sqlite_schema WHERE type=?1 AND name=?2 COLLATE NOCASE",
            [kind, object],
            |r| r.get(0),
        )
        .optional()?;
        // SQLite 3.53 rewrites source-trigger targets before invoking xRename.
        // Older SQLite invokes the callback first. Accept exactly that rewrite,
        // while requiring the remainder of every owned definition to match.
        let rewritten = rename.filter(|_| kind == "trigger").map(|new| {
            definition.replace(
                &format!("INSERT INTO {}(__ivm_source,", quote(name)),
                &format!("INSERT INTO {}(__ivm_source,", quote(new)),
            )
        });
        if actual.as_ref() != Some(definition) && (rewritten.is_none() || actual != rewritten) {
            return Err(error(format!(
                "managed object changed or missing: {object}"
            )));
        }
    }
    Ok(objects)
}

// xDestroy owns the DDL transaction and removes the virtual table itself.
pub fn uninstall(db: &Connection, name: &str) -> Result<()> {
    for (kind, object, _) in manifest(db, name, None)? {
        let keyword = match kind.as_str() {
            "trigger" => "TRIGGER",
            "index" => "INDEX",
            "table" => "TABLE",
            _ => return Err(error("invalid owned object type")),
        };
        census::batch(db, Phase::Teardown, name, &format!("DROP {keyword} main.{}", quote(&object)))?;
    }
    for table in ["__ivm_objects", "__ivm_columns", "__ivm_sources"] {
        census::exec(
            db,
            Phase::Teardown,
            name,
            &format!("DELETE FROM main.{table} WHERE view_name=?1"),
            [name],
        )?;
    }
    census::exec(
        db,
        Phase::Teardown,
        name,
        "DELETE FROM main.__ivm_schema WHERE id=(SELECT id FROM main.__ivm_views WHERE name=?1)",
        [name],
    )?;
    census::exec(
        db,
        Phase::Teardown,
        name,
        "DELETE FROM main.__ivm_views WHERE name=?1",
        [name],
    )?;
    Ok(())
}
