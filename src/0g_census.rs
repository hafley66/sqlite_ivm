//! One span per statement the engine hands to SQLite.
//!
//! The verb and kind come from the SQL's first keyword; the site comes from the
//! caller's `Location`. hafley-observe's SQLite trace emits a profile event
//! under the span, so the span carries the count and the event carries the
//! nanoseconds. Every helper here is `#[track_caller]`, so the span's `site`
//! names the real issuance site, never this file.
//!
//! With `--no-default-features` (extension builds) the spans are compiled out:
//! `open` returns a disabled span and nothing is recorded. That build is the
//! one the recipe times wall clock on.
#![cfg_attr(not(feature = "census"), allow(dead_code))]

use rusqlite::{Connection, Params, Result, Row};
#[cfg(feature = "census")]
use tracing::field;

/// The six points at which the engine hands SQL to SQLite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Declare,
    Drain,
    Maintain,
    Fixpoint,
    Materialize,
    Teardown,
}

impl Phase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Phase::Declare => "declare",
            Phase::Drain => "drain",
            Phase::Maintain => "maintain",
            Phase::Fixpoint => "fixpoint",
            Phase::Materialize => "materialize",
            Phase::Teardown => "teardown",
        }
    }
}

/// Whether the statement came off the connection's statement cache or was
/// prepared fresh. `prepare_cached`/`execute_cached` are the cached path;
/// `prepare`/`execute`/`execute_batch` are fresh.
pub(crate) const CACHED: &str = "cached";
pub(crate) const FRESH: &str = "fresh";

/// The first keyword of a statement, from the fixed vocabulary the engine
/// issues. A CTE (`WITH`) and an `EXPLAIN` are queries.
pub(crate) fn verb_of(sql: &str) -> &'static str {
    let head = sql.trim_start();
    let word: String = head
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match word.to_ascii_uppercase().as_str() {
        "CREATE" => "CREATE",
        "DROP" => "DROP",
        "ALTER" => "ALTER",
        "INSERT" => "INSERT",
        "UPDATE" => "UPDATE",
        "DELETE" => "DELETE",
        "SELECT" | "WITH" | "EXPLAIN" => "SELECT",
        "PRAGMA" => "PRAGMA",
        "SAVEPOINT" => "SAVEPOINT",
        "RELEASE" => "RELEASE",
        "ROLLBACK" => "ROLLBACK TO",
        _ => "OTHER",
    }
}

/// DDL changes the schema, DML changes rows, everything else reads.
pub(crate) fn kind_of(verb: &str) -> &'static str {
    match verb {
        "CREATE" | "DROP" | "ALTER" => "ddl",
        "INSERT" | "UPDATE" | "DELETE" => "dml",
        _ => "query",
    }
}

struct Site(&'static std::panic::Location<'static>);

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.0.file(), self.0.line())
    }
}

/// One open statement span. `rows` is filled once the statement returns.
pub(crate) struct Statement {
    span: tracing::Span,
}

impl Statement {
    pub(crate) fn enter(&self) -> tracing::span::Entered<'_> {
        self.span.enter()
    }

    pub(crate) fn rows(&self, rows: usize) {
        if !self.span.is_disabled() {
            self.span.record("rows", rows);
        }
    }
}

/// Opens the span for one statement site. `sql` names the verb; the fields
/// `kind`, `verb` and `site` are recorded only when a subscriber keeps them.
#[track_caller]
pub(crate) fn open(phase: Phase, object: &str, sql: &str, prepared: &'static str) -> Statement {
    #[cfg(feature = "census")]
    {
        let location = std::panic::Location::caller();
        let span = tracing::debug_span!(
            "stmt",
            phase = phase.as_str(),
            object = object,
            prepared = prepared,
            kind = field::Empty,
            verb = field::Empty,
            site = field::Empty,
            rows = field::Empty,
        );
        if !span.is_disabled() {
            let verb = verb_of(sql);
            span.record("kind", kind_of(verb));
            span.record("verb", verb);
            span.record("site", field::display(Site(location)));
        }
        Statement { span }
    }
    #[cfg(not(feature = "census"))]
    {
        let _ = (phase, object, sql, prepared);
        Statement {
            span: tracing::Span::none(),
        }
    }
}

#[track_caller]
pub(crate) fn batch(db: &Connection, phase: Phase, object: &str, sql: &str) -> Result<()> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let result = db.execute_batch(sql);
    statement.rows(0);
    result
}

#[track_caller]
pub(crate) fn exec<P: Params>(
    db: &Connection,
    phase: Phase,
    object: &str,
    sql: &str,
    params: P,
) -> Result<usize> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let rows = db.execute(sql, params)?;
    statement.rows(rows);
    Ok(rows)
}

#[track_caller]
pub(crate) fn exec_cached<P: Params>(
    db: &Connection,
    phase: Phase,
    object: &str,
    sql: &str,
    params: P,
) -> Result<usize> {
    let statement = open(phase, object, sql, CACHED);
    let _entered = statement.enter();
    let rows = db.prepare_cached(sql)?.execute(params)?;
    statement.rows(rows);
    Ok(rows)
}

#[track_caller]
pub(crate) fn query<T, P: Params, F: FnOnce(&Row<'_>) -> Result<T>>(
    db: &Connection,
    phase: Phase,
    object: &str,
    sql: &str,
    params: P,
    f: F,
) -> Result<T> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let value = db.query_row(sql, params, f)?;
    statement.rows(1);
    Ok(value)
}

#[track_caller]
pub(crate) fn query_cached<T, P: Params, F: FnOnce(&Row<'_>) -> Result<T>>(
    db: &Connection,
    phase: Phase,
    object: &str,
    sql: &str,
    params: P,
    f: F,
) -> Result<T> {
    let statement = open(phase, object, sql, CACHED);
    let _entered = statement.enter();
    let value = db.prepare_cached(sql)?.query_row(params, f)?;
    statement.rows(1);
    Ok(value)
}

#[track_caller]
pub(crate) fn query_map<T, P: Params, F: FnMut(&Row<'_>) -> Result<T>>(
    db: &Connection,
    phase: Phase,
    object: &str,
    sql: &str,
    params: P,
    f: F,
) -> Result<Vec<T>> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let mut prepared = db.prepare(sql)?;
    let rows = prepared.query_map(params, f)?.collect::<Result<Vec<T>>>()?;
    statement.rows(rows.len());
    Ok(rows)
}

/// Wraps one call that issues SQL through a bought crate (the collector's
/// shadow DDL), so its trace events nest under a census span too.
#[track_caller]
pub(crate) fn guard<T, F: FnOnce() -> Result<T>>(
    phase: Phase,
    object: &str,
    sql: &str,
    f: F,
) -> Result<T> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let value = f()?;
    statement.rows(0);
    Ok(value)
}

#[track_caller]
pub(crate) fn pragma<V: rusqlite::ToSql>(
    db: &Connection,
    phase: Phase,
    object: &str,
    name: &str,
    value: V,
) -> Result<()> {
    let statement = open(phase, object, &format!("PRAGMA {name}"), FRESH);
    let _entered = statement.enter();
    let result = db.pragma_update(None, name, value);
    statement.rows(0);
    result
}
