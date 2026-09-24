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
mod engine;
mod error;
mod meter;
mod observe;
mod plan;

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
    /// Creates the program's tables, indexes, views and source indexes, and
    /// registers the transaction collector, so source writes committed through
    /// SQL settle automatically at commit.
    pub fn install(
        conn: &rusqlite::Connection,
        name: &str,
        select_sql: &str,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            inner: catalog::install(conn, name, select_sql)?,
        })
    }

    /// Reload an installed program from the connection's catalog.
    pub fn open(conn: &rusqlite::Connection, name: &str) -> Result<Self, EngineError> {
        Ok(Self {
            inner: catalog::open(conn, name)?,
        })
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
