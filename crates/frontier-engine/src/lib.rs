//! SQLite-backed incremental frontier engine.
//!
//! One program (a SQL `SELECT` over source tables) stays maintained under
//! signed source changes. A caller hands the engine one complete frontier of
//! signed changes; the engine settles it — every derivation applied, the net
//! signed output change computed — before returning. Output row identity and
//! every join lookup are integer rowids served by SQLite indexes; cells keep
//! their storage class; no JSON, no stringified row keys.
//!
//! ```no_run
//! use frontier_engine::{Cell, Frontier, Program, Sign, SourceChange};
//! use sqlite_ext::rusqlite::Connection;
//!
//! # fn main() -> Result<(), frontier_engine::EngineError> {
//! let conn = Connection::open_in_memory().unwrap();
//! conn.execute_batch(
//!     "CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL)",
//! ).unwrap();
//! let program = Program::install(
//!     &conn,
//!     "team_cost",
//!     "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team",
//! )?;
//! let delta = program.settle(
//!     &conn,
//!     &[SourceChange::insert("job", [Cell::Integer(1), Cell::Integer(10), Cell::Integer(5)])],
//! )?;
//! assert_eq!(delta.len(), 1);
//! let rows = program.snapshot(&conn)?;
//! assert_eq!(rows.len(), 1);
//! program.teardown(&conn)?;
//! # Ok(())
//! # }
//! ```

use sqlite_ext::rusqlite;

mod catalog;
mod composition;
mod engine;
mod error;
mod meter;
mod observe;
mod plan;

pub use composition::Composition;
pub use error::{EngineError, ErrorKind, Stage};

/// One typed SQLite value. The engine preserves the storage class exactly:
/// an integer stays an integer, text stays text, across staging, joins and
/// output.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl rusqlite::ToSql for Cell {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::ValueRef;
        use rusqlite::types::{ToSqlOutput, Value};
        let borrowed: ValueRef<'_> = match self {
            Cell::Null => ValueRef::Null,
            Cell::Integer(v) => ValueRef::Integer(*v),
            Cell::Real(v) => ValueRef::Real(*v),
            Cell::Text(v) => ValueRef::Text(v.as_bytes()),
            Cell::Blob(v) => ValueRef::Blob(v),
        };
        // Cell owns its payload, so the borrowed reference is safely turned
        // into the owned Value the ToSql contract wants.
        let owned = match borrowed {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => Value::Integer(v),
            ValueRef::Real(v) => Value::Real(v),
            ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Value::Blob(b.to_vec()),
        };
        Ok(ToSqlOutput::Owned(owned))
    }
}

/// A typed row, in declared column order.
#[derive(Clone, Debug, PartialEq)]
pub struct Tuple(pub Vec<Cell>);

/// Direction of one signed change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    Insert,
    Delete,
}

impl Sign {
    pub const fn as_integer(self) -> i64 {
        match self {
            Sign::Insert => 1,
            Sign::Delete => -1,
        }
    }
}

/// One signed source change: the full row image of one source row, in the
/// relation's declared column order.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceChange {
    pub relation: String,
    pub sign: Sign,
    pub row: Vec<Cell>,
}

impl SourceChange {
    pub fn insert(relation: impl Into<String>, row: impl IntoIterator<Item = Cell>) -> Self {
        Self {
            relation: relation.into(),
            sign: Sign::Insert,
            row: row.into_iter().collect(),
        }
    }

    pub fn delete(relation: impl Into<String>, row: impl IntoIterator<Item = Cell>) -> Self {
        Self {
            relation: relation.into(),
            sign: Sign::Delete,
            row: row.into_iter().collect(),
        }
    }
}

/// One output column of an installed program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputColumn {
    pub name: String,
}

/// One net signed output row. The row is the program's output schema; it is
/// emitted only when its visible form changed at the settled frontier.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputChange {
    pub sign: Sign,
    pub row: Vec<Cell>,
}

/// One installed program: the compiled plan plus its installed objects.
///
/// The handle owns no connection: all state lives in the connection's schema
/// and catalog, so the same program can be driven from a second handle opened
/// with [`Program::open`]. The public trait is the whole contract; the
/// loadable extension drives the same methods from its commit callback.
pub struct Program {
    inner: std::sync::Arc<catalog::Installed>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("name", &self.inner.name)
            .finish()
    }
}

impl Program {
    /// Parse, compile, validate and install `select_sql` under `name`.
    /// Creates the program's tables, indexes, views and source indexes,
    /// materializes the rows the sources already hold (the installed
    /// snapshot equals a fresh evaluation of `select_sql` over the current
    /// tables; the frontier counter stays 0 and the delta table stays
    /// empty), and registers the transaction collector, so source writes
    /// committed through SQL settle automatically at commit.
    pub fn install(
        conn: &rusqlite::Connection,
        name: &str,
        select_sql: &str,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            inner: catalog::install(conn, name, select_sql, catalog::Watch::Sources)?,
        })
    }

    /// Install without a commit collector: composition's entry for programs
    /// that settle only through the explicit API. Materializes the rows the
    /// sources already hold, exactly like [`Program::install`](Self::install).
    pub(crate) fn install_unwatched(
        conn: &rusqlite::Connection,
        name: &str,
        select_sql: &str,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            inner: catalog::install(conn, name, select_sql, catalog::Watch::None)?,
        })
    }

    /// Reload an installed program from the connection's catalog.
    pub fn open(conn: &rusqlite::Connection, name: &str) -> Result<Self, EngineError> {
        Ok(Self {
            inner: catalog::open(conn, name)?,
        })
    }

    /// Opens a standalone program on a new connection and restores its
    /// persisted source collector before any source-table write occurs.
    pub fn reattach(conn: &rusqlite::Connection, name: &str) -> Result<Self, EngineError> {
        let program = Self::open(conn, name)?;
        if !program.inner.derived.is_empty() {
            return Err(EngineError::unsupported(
                Stage::Install,
                name,
                "a composed program must reattach with its producer",
            ));
        }
        engine::reattach_collector(conn, &program.inner)?;
        Ok(program)
    }

    /// The program name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The output schema: one entry per output column.
    pub fn output_columns(&self) -> &[OutputColumn] {
        &self.inner.output
    }

    /// The source tables the program watches.
    pub fn sources(&self) -> &[String] {
        &self.inner.sources
    }

    /// Composition access to the install record.
    pub(crate) fn handle(&self) -> std::sync::Arc<catalog::Installed> {
        std::sync::Arc::clone(&self.inner)
    }

    /// The visible output view this program serves: `frontier_<name>`.
    pub fn view(&self) -> String {
        catalog::view(&self.inner.name)
    }
}

/// Restores every installed frontier collector after the extension loads on
/// a fresh connection. The dependency catalog identifies composed pairs;
/// remaining programs each own one standalone collector. This reads only
/// schema/catalog rows and registers connection-local callback state.
pub fn reattach_database(conn: &rusqlite::Connection) -> Result<(), EngineError> {
    use std::collections::HashSet;

    let exists = |table: &str| -> Result<bool, EngineError> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
        .map_err(|e| EngineError::new(Stage::Install, table, ErrorKind::Sqlite(e.to_string())))
    };
    if !exists("frontier_catalog")? {
        return Ok(());
    }

    let mut paired = HashSet::new();
    if exists("frontier_dependency")? {
        let mut query = conn
            .prepare("SELECT producer, consumer FROM frontier_dependency ORDER BY producer")
            .map_err(|e| {
                EngineError::new(
                    Stage::Install,
                    "frontier_dependency",
                    ErrorKind::Sqlite(e.to_string()),
                )
            })?;
        let edges = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| {
                EngineError::new(
                    Stage::Install,
                    "frontier_dependency",
                    ErrorKind::Sqlite(e.to_string()),
                )
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| {
                EngineError::new(
                    Stage::Install,
                    "frontier_dependency",
                    ErrorKind::Sqlite(e.to_string()),
                )
            })?;
        drop(query);
        for (producer, consumer) in edges {
            Composition::reattach(conn, &producer, &consumer)?;
            paired.insert(producer);
            paired.insert(consumer);
        }
    }

    let mut query = conn
        .prepare("SELECT name FROM frontier_catalog ORDER BY name")
        .map_err(|e| {
            EngineError::new(
                Stage::Install,
                "frontier_catalog",
                ErrorKind::Sqlite(e.to_string()),
            )
        })?;
    let names = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| {
            EngineError::new(
                Stage::Install,
                "frontier_catalog",
                ErrorKind::Sqlite(e.to_string()),
            )
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| {
            EngineError::new(
                Stage::Install,
                "frontier_catalog",
                ErrorKind::Sqlite(e.to_string()),
            )
        })?;
    drop(query);
    for name in names {
        if !paired.contains(&name) {
            Program::reattach(conn, &name)?;
        }
    }
    Ok(())
}

/// The frontier contract: settle one complete batch of signed source changes,
/// expose the stable snapshot, and tear the program down.
///
/// `settle` returns only after the frontier is settled; its result is the net
/// signed output change of that frontier. Every method takes the connection
/// explicitly: the program handle holds no borrow, so the extension path can
/// open a program per commit without lifetime coupling.
pub trait Frontier {
    /// Settle one complete frontier. Atomic: either every source change lands
    /// and the net output change is returned, or the connection keeps its
    /// previous committed state and the error names the stage and relation.
    fn settle(
        &self,
        conn: &rusqlite::Connection,
        batch: &[SourceChange],
    ) -> Result<Vec<OutputChange>, EngineError>;

    /// The stable snapshot: visible output rows in a deterministic order.
    fn snapshot(&self, conn: &rusqlite::Connection) -> Result<Vec<Tuple>, EngineError>;

    /// The identifier of the last settled frontier: monotone per program,
    /// bumped once per settled batch, unchanged by rolled-back frontiers.
    fn frontier_id(&self, conn: &rusqlite::Connection) -> Result<u64, EngineError>;

    /// Remove every installed object and the collector. Separate from settle:
    /// source tables and their rows are left untouched.
    fn teardown(&self, conn: &rusqlite::Connection) -> Result<(), EngineError>;
}

impl Frontier for Program {
    fn settle(
        &self,
        conn: &rusqlite::Connection,
        batch: &[SourceChange],
    ) -> Result<Vec<OutputChange>, EngineError> {
        engine::settle(conn, &self.inner, batch, false)
    }

    fn snapshot(&self, conn: &rusqlite::Connection) -> Result<Vec<Tuple>, EngineError> {
        engine::snapshot(conn, &self.inner)
    }

    fn frontier_id(&self, conn: &rusqlite::Connection) -> Result<u64, EngineError> {
        engine::frontier_id(conn, &self.inner)
    }

    fn teardown(&self, conn: &rusqlite::Connection) -> Result<(), EngineError> {
        catalog::teardown(conn, &self.inner)
    }
}
