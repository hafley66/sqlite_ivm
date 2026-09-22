//! One span per statement the engine hands to SQLite.
//!
//! The verb and kind come from the SQL's first keyword; the site comes from the
//! caller's `Location`. hafley-observe's SQLite trace emits a profile event
//! under the span, so the span carries the count and the event carries the
//! nanoseconds. Every helper here is `#[track_caller]`, so the span's `site`
//! names the real issuance site, never this file.
//!
//! Statement spans remain available in both linked and native-extension builds.
//! Runtime filters select their output through the existing hafley-observe layers.

use rusqlite::{Connection, Params, Result, Row};
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
/// issues. A CTE (`WITH`) is a query; `EXPLAIN` keeps its own verb so the plan
/// probes stay visible; a rollback says whether it targets a savepoint.
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
        "SELECT" | "WITH" => "SELECT",
        "EXPLAIN" => "EXPLAIN",
        "PRAGMA" => "PRAGMA",
        "SAVEPOINT" => "SAVEPOINT",
        "RELEASE" => "RELEASE",
        "ROLLBACK" => {
            let rest = head[word.len()..].trim_start();
            if rest.len() >= 2 && rest[..2].eq_ignore_ascii_case("TO") {
                "ROLLBACK TO"
            } else {
                "ROLLBACK"
            }
        }
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

/// One open statement span. `rows` is filled once the statement returns, and
/// only when the count is known: a DML statement reports `changes()`, a query
/// reports the rows it returned. `batch`, `guard` and `pragma` leave it
/// unrecorded, so the table shows a blank rather than a false zero.
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

}

#[track_caller]
pub(crate) fn batch(db: &Connection, phase: Phase, object: &str, sql: &str) -> Result<()> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    let operation = tracing::debug_span!("execute_batch", sql, sql_bytes = sql.len());
    let _operation = operation.enter();
    tracing::debug!("batch_start");
    let result = db.execute_batch(sql);
    tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "batch_end");
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
    let mut prepared = {
        let _prepare = tracing::debug_span!("prepare", sql, sql_bytes = sql.len(), cached = false).entered();
        tracing::debug!("prepare_start");
        let result = db.prepare(sql);
        tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "prepare_end");
        result?
    };
    let _execute = tracing::debug_span!("execute").entered();
    tracing::debug!("execute_start");
    let result = prepared.execute(params);
    tracing::debug!(rows = ?result.as_ref().ok(), error = ?result.as_ref().err(), "execute_end");
    let rows = result?;
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
    let mut prepared = {
        let _prepare = tracing::debug_span!("prepare", sql, sql_bytes = sql.len(), cached = true).entered();
        tracing::debug!("prepare_start");
        let result = db.prepare_cached(sql);
        tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "prepare_end");
        result?
    };
    let _execute = tracing::debug_span!("execute").entered();
    tracing::debug!("execute_start");
    let result = prepared.execute(params);
    tracing::debug!(rows = ?result.as_ref().ok(), error = ?result.as_ref().err(), "execute_end");
    let rows = result?;
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
    let mut prepared = {
        let _prepare = tracing::debug_span!("prepare", sql, sql_bytes = sql.len(), cached = false).entered();
        tracing::debug!("prepare_start");
        let result = db.prepare(sql);
        tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "prepare_end");
        result?
    };
    let _execute = tracing::debug_span!("execute").entered();
    tracing::debug!("execute_start");
    let result = prepared.query_row(params, f);
    tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "execute_end");
    let value = result?;
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
    let mut prepared = {
        let _prepare = tracing::debug_span!("prepare", sql, sql_bytes = sql.len(), cached = true).entered();
        tracing::debug!("prepare_start");
        let result = db.prepare_cached(sql);
        tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "prepare_end");
        result?
    };
    let _execute = tracing::debug_span!("execute").entered();
    tracing::debug!("execute_start");
    let result = prepared.query_row(params, f);
    tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "execute_end");
    let value = result?;
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
    let mut prepared = {
        let _prepare = tracing::debug_span!("prepare", sql, sql_bytes = sql.len(), cached = false).entered();
        tracing::debug!("prepare_start");
        let result = db.prepare(sql);
        tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "prepare_end");
        result?
    };
    let _execute = tracing::debug_span!("execute").entered();
    tracing::debug!("execute_start");
    let result = prepared.query_map(params, f).and_then(|rows| rows.collect::<Result<Vec<T>>>());
    tracing::debug!(rows = ?result.as_ref().ok().map(Vec::len), error = ?result.as_ref().err(), "execute_end");
    let rows = result?;
    statement.rows(rows.len());
    Ok(rows)
}

/// Wraps one call that issues SQL through a bought crate (the collector's
/// shadow DDL), so its trace events nest under a statement span too.
#[track_caller]
pub(crate) fn guard<T, F: FnOnce() -> Result<T>>(
    phase: Phase,
    object: &str,
    sql: &str,
    f: F,
) -> Result<T> {
    let statement = open(phase, object, sql, FRESH);
    let _entered = statement.enter();
    tracing::debug!(sql, "guard_start");
    let result = f();
    tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "guard_end");
    result
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
    tracing::debug!(name, "pragma_start");
    let result = db.pragma_update(None, name, value);
    tracing::debug!(success = result.is_ok(), error = ?result.as_ref().err(), "pragma_end");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use hafley_observe::CountRecorder;
    use tracing_subscriber::prelude::*;

    #[test]
    fn preparation_and_execution_report_success_and_failure_separately() -> Result<()> {
        let (recorder, layer) = CountRecorder::new();
        let _guard = tracing_subscriber::registry().with(layer).set_default();
        let db = Connection::open_in_memory()?;
        exec(&db, Phase::Declare, "telemetry", "CREATE TABLE t(x UNIQUE)", [])?;
        exec_cached(&db, Phase::Maintain, "telemetry", "INSERT INTO t VALUES(?1)", [7])?;
        assert!(exec_cached(&db, Phase::Maintain, "telemetry", "INSERT INTO t VALUES(?1)", [7]).is_err());
        assert!(exec(&db, Phase::Maintain, "telemetry", "INSERT INTO missing VALUES(1)", []).is_err());
        assert_eq!(query(&db, Phase::Maintain, "telemetry", "SELECT x FROM t", [], |r| r.get::<_, i64>(0))?, 7);
        assert_eq!(query_cached(&db, Phase::Maintain, "telemetry", "SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))?, 1);
        assert_eq!(query_map(&db, Phase::Maintain, "telemetry", "SELECT x FROM t", [], |r| r.get::<_, i64>(0))?, vec![7]);
        let events = recorder.event_sums("sqlite_ivm::statements", tracing::Level::DEBUG, "stmt", "object", Some("message"));
        let counts = events.into_iter().map(|((object, message), sums)| {
            assert_eq!(object, "telemetry");
            (message, sums.events)
        }).collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(counts, std::collections::BTreeMap::from([
            ("execute_end".into(), 6), ("execute_start".into(), 6),
            ("prepare_end".into(), 7), ("prepare_start".into(), 7),
        ]));
        let spans = recorder.counts();
        assert_eq!(spans.instances["prepare"], 7);
        assert_eq!(spans.instances["execute"], 6);
        Ok(())
    }
}
