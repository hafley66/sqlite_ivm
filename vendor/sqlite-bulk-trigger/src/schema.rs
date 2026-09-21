use rusqlite::{Connection, Result};

/// Column 0 of the declaration is the only visible one. Hidden columns start
/// after it, and `Inserts` puts the two rowid arguments before column 0.
pub(crate) const SOURCE_COLUMN: usize = 1;
pub(crate) const SIGN_COLUMN: usize = 2;
pub(crate) const FIRST_VALUE_COLUMN: usize = 3;
pub(crate) const ROWID_ARGUMENTS: usize = 2;

pub(crate) fn error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ModuleError(message.into())
}

pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The collector name doubles as its module name, which SQLite matches as a
/// bare identifier in `USING`.
pub(crate) fn check_identifier(name: &str) -> Result<()> {
    let head = name.chars().next();
    let shaped = head.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if shaped {
        return Ok(());
    }
    Err(error(format!(
        "collector name {name:?} must be an ASCII identifier"
    )))
}

pub(crate) fn delta_name(collector: &str) -> String {
    format!("{collector}_delta")
}

/// Value columns carry no declared type. SQLite applies column affinity to the
/// arguments it hands xUpdate, and any affinity would rewrite the row's types.
pub(crate) fn declaration(width: usize) -> String {
    let values = (0..width)
        .map(|at| format!(",__value{at} HIDDEN"))
        .collect::<String>();
    format!("CREATE TABLE x(staged_rows INTEGER,__source HIDDEN,__sign HIDDEN{values})")
}

pub(crate) fn create_delta(collector: &str, width: usize) -> String {
    let values = (0..width)
        .map(|at| format!(",value{at}"))
        .collect::<String>();
    format!(
        "CREATE TABLE main.{}(sequence INTEGER PRIMARY KEY,source TEXT NOT NULL,\
         sign INTEGER NOT NULL,width INTEGER NOT NULL{values})",
        quote(&delta_name(collector))
    )
}

pub(crate) fn insert_delta(collector: &str, width: usize) -> String {
    let names = (0..width)
        .map(|at| format!(",value{at}"))
        .collect::<String>();
    let binds = (0..width + 4)
        .map(|at| format!("?{}", at + 1))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "INSERT INTO main.{}(sequence,source,sign,width{names}) VALUES({binds})",
        quote(&delta_name(collector))
    )
}

pub(crate) fn select_delta(collector: &str, width: usize) -> String {
    let names = (0..width)
        .map(|at| format!(",value{at}"))
        .collect::<String>();
    format!(
        "SELECT sequence,source,sign,width{names} FROM main.{} ORDER BY sequence",
        quote(&delta_name(collector))
    )
}

pub(crate) fn delete_delta(collector: &str) -> String {
    format!("DELETE FROM main.{}", quote(&delta_name(collector)))
}

pub(crate) fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    let names = db
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() {
        return Err(error(format!("watched table {table:?} has no columns")));
    }
    Ok(names)
}

fn trigger_name(collector: &str, table: &str, event: &str) -> String {
    quote(&format!("{collector}_{table}_{event}"))
}

fn body(collector: &str, table: &str, columns: &[String], image: &str, sign: i64) -> String {
    let names = (0..columns.len())
        .map(|at| format!(",__value{at}"))
        .collect::<String>();
    let reads = columns
        .iter()
        .map(|column| format!(",{image}.{}", quote(column)))
        .collect::<String>();
    format!(
        "INSERT INTO {}(__source,__sign{names}) VALUES({},{sign}{reads});",
        quote(collector),
        literal(table)
    )
}

/// AFTER triggers only. A BEFORE trigger would stage a row that a later
/// constraint failure removes from the source.
pub(crate) fn create_triggers(collector: &str, table: &str, columns: &[String]) -> String {
    let insert = body(collector, table, columns, "NEW", 1);
    let delete = body(collector, table, columns, "OLD", -1);
    format!(
        "CREATE TRIGGER main.{} AFTER INSERT ON {} BEGIN {insert} END;\n\
         CREATE TRIGGER main.{} AFTER DELETE ON {} BEGIN {delete} END;\n\
         CREATE TRIGGER main.{} AFTER UPDATE ON {} BEGIN {delete} {insert} END;",
        trigger_name(collector, table, "insert"),
        quote(table),
        trigger_name(collector, table, "delete"),
        quote(table),
        trigger_name(collector, table, "update"),
        quote(table),
    )
}

pub(crate) fn drop_triggers(collector: &str, tables: &[String]) -> String {
    tables
        .iter()
        .flat_map(|table| {
            ["insert", "delete", "update"]
                .into_iter()
                .map(move |event| {
                    format!("DROP TRIGGER IF EXISTS main.{};", trigger_name(collector, table, event))
                })
        })
        .collect()
}
