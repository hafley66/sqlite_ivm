use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::{Connection, StatementStatus};
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::flush::{Flush, Row, Sink, Writer};

pub const SQLITE_TARGET: &str = "sqlite";

/// The file the log sink writes when a host did not name one.
pub const LOG_PATH_VARIABLE: &str = "HAFLEY_LOG_SQLITE";

/// How the log sink keys its repeated columns.
pub const LOG_ENCODING_VARIABLE: &str = "HAFLEY_LOG_SQLITE_ENCODING";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatementCounters {
    pub vm_step: i32,
    pub fullscan_step: i32,
    pub sort: i32,
    pub autoindex: i32,
    pub reprepare: i32,
    pub run: i32,
    pub mem_used: i32,
}

/// What each counter means when it is nonzero. SQLite documents these as
/// opaque integers; the names below are the reason anyone would read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementFinding {
    TableScan,
    TemporaryBtreeSort,
    RuntimeIndexBuilt,
    SchemaChangedUnderStatement,
}

impl StatementFinding {
    pub fn as_str(self) -> &'static str {
        match self {
            StatementFinding::TableScan => "table scan, no index served this statement",
            StatementFinding::TemporaryBtreeSort => {
                "temporary b-tree sort, an ORDER BY or GROUP BY had no index"
            }
            StatementFinding::RuntimeIndexBuilt => {
                "transient index built at run time, a permanent index is missing"
            }
            StatementFinding::SchemaChangedUnderStatement => {
                "statement was reprepared, the schema changed beneath it"
            }
        }
    }
}

impl StatementCounters {
    pub fn read(statement: &rusqlite::trace::StmtRef<'_>) -> Self {
        StatementCounters {
            vm_step: statement.get_status(StatementStatus::VmStep),
            fullscan_step: statement.get_status(StatementStatus::FullscanStep),
            sort: statement.get_status(StatementStatus::Sort),
            autoindex: statement.get_status(StatementStatus::AutoIndex),
            reprepare: statement.get_status(StatementStatus::RePrepare),
            run: statement.get_status(StatementStatus::Run),
            mem_used: statement.get_status(StatementStatus::MemUsed),
        }
    }

    pub fn findings(&self) -> Vec<StatementFinding> {
        let mut findings = Vec::new();
        if self.fullscan_step > 0 {
            findings.push(StatementFinding::TableScan);
        }
        if self.sort > 0 {
            findings.push(StatementFinding::TemporaryBtreeSort);
        }
        if self.autoindex > 0 {
            findings.push(StatementFinding::RuntimeIndexBuilt);
        }
        if self.reprepare > 0 {
            findings.push(StatementFinding::SchemaChangedUnderStatement);
        }
        findings
    }
}

/// `vm_step` is the deterministic cost of a statement: the same input yields
/// the same count on every machine, unlike elapsed time.
fn emit(event: TraceEvent<'_>) {
    match event {
        TraceEvent::Stmt(statement, expanded) => {
            tracing::trace!(
                target: SQLITE_TARGET,
                sql = %statement.sql(),
                expanded = %expanded,
                "statement begins"
            );
        }
        TraceEvent::Profile(statement, elapsed) => {
            let counters = StatementCounters::read(&statement);
            let findings = counters.findings();
            tracing::debug!(
                target: SQLITE_TARGET,
                sql = %statement.sql(),
                nanos = elapsed.as_nanos() as u64,
                vm_step = counters.vm_step,
                fullscan_step = counters.fullscan_step,
                sort = counters.sort,
                autoindex = counters.autoindex,
                reprepare = counters.reprepare,
                run = counters.run,
                mem_used = counters.mem_used,
                "statement finished"
            );
            for finding in findings {
                tracing::warn!(
                    target: SQLITE_TARGET,
                    sql = %statement.sql(),
                    vm_step = counters.vm_step,
                    "{}",
                    finding.as_str()
                );
            }
        }
        _ => {}
    }
}

pub fn instrument(connection: &Connection) {
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
        Some(emit),
    );
}

pub fn silence(connection: &Connection) {
    connection.trace_v2(TraceEventCodes::empty(), None);
}

/// The planner's own account of a statement, one row per plan node.
pub fn query_plan(connection: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
        .query_map([], |row| row.get::<_, String>(3))?
        .collect()
}

/// The repeating columns of a log record, each stored once with a surrogate
/// key. The event row carries the integer.
pub const DICTIONARY_DDL: &str = "\
CREATE TABLE IF NOT EXISTS log_span(id INTEGER PRIMARY KEY, name TEXT NOT NULL, UNIQUE(name));\
CREATE TABLE IF NOT EXISTS log_target(id INTEGER PRIMARY KEY, target TEXT NOT NULL, UNIQUE(target));\
CREATE TABLE IF NOT EXISTS log_file(id INTEGER PRIMARY KEY, file TEXT NOT NULL, UNIQUE(file));\
CREATE TABLE IF NOT EXISTS log_level(id INTEGER PRIMARY KEY, level TEXT NOT NULL, UNIQUE(level));\
CREATE TABLE IF NOT EXISTS log_field(id INTEGER PRIMARY KEY, field TEXT NOT NULL, UNIQUE(field));\
CREATE TABLE IF NOT EXISTS log_event(id INTEGER PRIMARY KEY, ts_ns INTEGER NOT NULL, \
span_id INTEGER NOT NULL REFERENCES log_span(id), \
target_id INTEGER NOT NULL REFERENCES log_target(id), \
file_id INTEGER NOT NULL REFERENCES log_file(id), \
line INTEGER NOT NULL, \
level_id INTEGER NOT NULL REFERENCES log_level(id));\
CREATE TABLE IF NOT EXISTS log_value(event_id INTEGER NOT NULL REFERENCES log_event(id), \
field_id INTEGER NOT NULL REFERENCES log_field(id), value TEXT NOT NULL, \
PRIMARY KEY(event_id, field_id)) WITHOUT ROWID;";

/// The same shape with every key inlined. It exists to price the dictionary
/// against repetition, and shares the event and value shapes so the only
/// difference between the two sinks is where the repeated strings live.
pub const TEXT_DDL: &str = "\
CREATE TABLE IF NOT EXISTS log_event(id INTEGER PRIMARY KEY, ts_ns INTEGER NOT NULL, \
span TEXT NOT NULL, target TEXT NOT NULL, file TEXT NOT NULL, \
line INTEGER NOT NULL, level TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS log_value(event_id INTEGER NOT NULL, field TEXT NOT NULL, \
value TEXT NOT NULL, PRIMARY KEY(event_id, field)) WITHOUT ROWID;";

const DICTIONARY_COLUMNS: [(&str, &str); 5] = [
    ("log_span", "name"),
    ("log_target", "target"),
    ("log_file", "file"),
    ("log_level", "level"),
    ("log_field", "field"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Dictionary,
    Text,
}

/// The relational sink. One writer, two encodings, chosen at construction.
pub struct LogSink {
    encoding: Encoding,
    connection: Mutex<Connection>,
    cache: Mutex<HashMap<&'static str, HashMap<String, i64>>>,
}

impl LogSink {
    pub fn dictionary(connection: Connection) -> Self {
        Self::new(Encoding::Dictionary, connection)
    }

    pub fn text(connection: Connection) -> Self {
        Self::new(Encoding::Text, connection)
    }

    fn new(encoding: Encoding, connection: Connection) -> Self {
        Self {
            encoding,
            connection: Mutex::new(connection),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Rows in the two log tables, for the receipts.
    pub fn rows(&self) -> i64 {
        let connection = self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = |table: &str| -> i64 {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))
                .unwrap_or_default()
        };
        count("log_event") + count("log_value")
    }

    /// Bytes the database occupies, for the receipts.
    pub fn bytes(&self) -> i64 {
        let connection = self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let page_count: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap_or_default();
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap_or_default();
        page_count * page_size
    }

    fn write_dictionary(&self, connection: &Connection, rows: &[Row]) -> rusqlite::Result<()> {
        let mut interners: Vec<Interner<'_, '_>> = DICTIONARY_COLUMNS
            .iter()
            .map(|(table, column)| Interner::new(connection, &self.cache, table, column))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut event = connection.prepare(
            "INSERT INTO log_event(ts_ns, span_id, target_id, file_id, line, level_id) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut value =
            connection.prepare("INSERT INTO log_value(event_id, field_id, value) VALUES(?1, ?2, ?3)")?;
        for row in rows {
            let span_id = interners[0].id(&row.name)?;
            let target_id = interners[1].id(&row.target)?;
            let file_id = interners[2].id(&row.file)?;
            let level_id = interners[3].id(row.level)?;
            event.execute(rusqlite::params![
                row.ts_ns, span_id, target_id, file_id, row.line, level_id
            ])?;
            let event_id = connection.last_insert_rowid();
            for (field, text) in &row.fields {
                let field_id = interners[4].id(field)?;
                value.execute(rusqlite::params![event_id, field_id, text])?;
            }
        }
        Ok(())
    }

    fn write_text(&self, connection: &Connection, rows: &[Row]) -> rusqlite::Result<()> {
        let mut event = connection.prepare(
            "INSERT INTO log_event(ts_ns, span, target, file, line, level) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut value =
            connection.prepare("INSERT INTO log_value(event_id, field, value) VALUES(?1, ?2, ?3)")?;
        for row in rows {
            event.execute(rusqlite::params![
                row.ts_ns,
                row.name,
                row.target,
                row.file,
                row.line,
                row.level
            ])?;
            let event_id = connection.last_insert_rowid();
            for (field, text) in &row.fields {
                value.execute(rusqlite::params![event_id, field, text])?;
            }
        }
        Ok(())
    }
}

impl Sink for LogSink {
    fn label(&self) -> &'static str {
        match self.encoding {
            Encoding::Dictionary => "sqlite-dictionary",
            Encoding::Text => "sqlite-text",
        }
    }

    fn write(&self, rows: &[Row]) {
        let connection = self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = connection
            .execute_batch("BEGIN")
            .and_then(|()| match self.encoding {
                Encoding::Dictionary => self.write_dictionary(&connection, rows),
                Encoding::Text => self.write_text(&connection, rows),
            })
            .and_then(|()| connection.execute_batch("COMMIT"));
        if let Err(error) = result {
            // A refused batch must be visible by name, not by silence.
            tracing::error!(target: SQLITE_TARGET, %error, rows = rows.len(), "log batch refused");
            let _ = connection.execute_batch("ROLLBACK");
        }
    }
}

/// The schema for an encoding.
pub fn schema(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Dictionary => DICTIONARY_DDL,
        Encoding::Text => TEXT_DDL,
    }
}

/// The open sink and its writer. Both are shared with the layer.
pub struct Log {
    pub sink: Arc<LogSink>,
    pub writer: Arc<Writer>,
}

/// Open the log database and apply the schema the encoding needs.
pub fn open(path: &Path, encoding: Encoding, flush: Flush) -> rusqlite::Result<Log> {
    let connection = Connection::open(path)?;
    connection.execute_batch(schema(encoding))?;
    let sink = Arc::new(match encoding {
        Encoding::Dictionary => LogSink::dictionary(connection),
        Encoding::Text => LogSink::text(connection),
    });
    let writer = Arc::new(Writer::new(Arc::clone(&sink) as Arc<dyn Sink>, flush));
    Ok(Log { sink, writer })
}

/// The log sink as a layer, driven by `HAFLEY_LOG_SQLITE`. Unset, no sink.
pub fn log_layer<S>(flush: Flush) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let path = std::env::var(LOG_PATH_VARIABLE)
        .ok()
        .filter(|path| !path.is_empty())?;
    let encoding = match std::env::var(LOG_ENCODING_VARIABLE).as_deref() {
        Ok("text") => Encoding::Text,
        _ => Encoding::Dictionary,
    };
    let log = open(Path::new(&path), encoding, flush).ok()?;
    Some(crate::sink::SinkLayer::new(log.writer).boxed())
}

/// Interns one column into its side table and remembers the id.
struct Interner<'conn, 'cache> {
    insert: rusqlite::Statement<'conn>,
    select: rusqlite::Statement<'conn>,
    table: &'static str,
    cache: &'cache Mutex<HashMap<&'static str, HashMap<String, i64>>>,
}

impl<'conn, 'cache> Interner<'conn, 'cache> {
    fn new(
        connection: &'conn Connection,
        cache: &'cache Mutex<HashMap<&'static str, HashMap<String, i64>>>,
        table: &'static str,
        column: &'static str,
    ) -> rusqlite::Result<Self> {
        Ok(Self {
            insert: connection
                .prepare(&format!("INSERT OR IGNORE INTO {table}({column}) VALUES(?1)"))?,
            select: connection.prepare(&format!("SELECT id FROM {table} WHERE {column} = ?1"))?,
            table,
            cache,
        })
    }

    /// The key repeats across many rows, so a hit costs a hash lookup and a
    /// miss costs one insert plus one select. UNIQUE is the dedup.
    fn id(&mut self, key: &str) -> rusqlite::Result<i64> {
        {
            let cache = self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(id) = cache.get(self.table).and_then(|ids| ids.get(key)) {
                return Ok(*id);
            }
        }
        self.insert.execute([key])?;
        let id = self.select.query_row([key], |row| row.get::<_, i64>(0))?;
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries: usize = cache.values().map(HashMap::len).sum();
        if entries >= crate::flush::DICTIONARY_CACHE_BOUND {
            cache.clear();
        }
        cache.entry(self.table).or_default().insert(key.to_owned(), id);
        Ok(id)
    }
}
