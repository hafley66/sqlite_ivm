//! The error contract: every failure names its stage and the relation involved.

use std::fmt;

use sqlite_ext::rusqlite;

/// Where in the pipeline a failure happened. The stage travels in every
/// [`EngineError`], across the extension boundary inside the message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The program SQL did not tokenize or parse.
    Parse,
    /// The parse tree did not compile to a supported plan, or referenced a
    /// missing table or column.
    Plan,
    /// Installing or reloading a program failed.
    Install,
    /// Tearing a program down failed.
    Teardown,
    /// A source-change batch did not match the program's sources.
    Collect,
    /// Maintenance over one settled frontier failed.
    Settle,
    /// Reading a snapshot or delta failed.
    Read,
}

impl Stage {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Stage::Parse => "parse",
            Stage::Plan => "plan",
            Stage::Install => "install",
            Stage::Teardown => "teardown",
            Stage::Collect => "collect",
            Stage::Settle => "settle",
            Stage::Read => "read",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What class of failure it is, independent of the wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// A required shape the engine does not implement. Never a silent
    /// fallback: the caller sees this instead of a recomputation.
    Unsupported(&'static str),
    /// A batch named a relation the program does not watch.
    UnknownRelation(String),
    /// A table or plan reference named a column that does not exist.
    UnknownColumn(String),
    /// A batch row's arity did not match its relation's arity.
    Arity {
        relation: String,
        expected: usize,
        got: usize,
    },
    /// Install/teardown state conflict, e.g. a name already installed.
    State(String),
    /// A SQLite statement failed under the engine.
    Sqlite(String),
    /// A LetRec with `limit` still changed a variable in body evaluation `limit + 1`; `rel` is
    /// the LetRec's first id.
    LetRecLimit { rel: Option<u32>, limit: u32 },
}

#[derive(Clone, Debug)]
pub struct EngineError {
    pub stage: Stage,
    pub relation: String,
    pub kind: ErrorKind,
}

impl EngineError {
    pub fn new(stage: Stage, relation: impl Into<String>, kind: ErrorKind) -> Self {
        Self {
            stage,
            relation: relation.into(),
            kind,
        }
    }

    pub fn unsupported(stage: Stage, relation: impl Into<String>, why: &'static str) -> Self {
        Self::new(stage, relation, ErrorKind::Unsupported(why))
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind, ErrorKind::Unsupported(_))
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}/{}] {}", self.stage, self.relation, self.kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::Unsupported(why) => write!(f, "unsupported: {why}"),
            ErrorKind::UnknownRelation(name) => write!(f, "unknown relation {name:?}"),
            ErrorKind::UnknownColumn(name) => write!(f, "unknown column {name:?}"),
            ErrorKind::Arity {
                relation,
                expected,
                got,
            } => {
                write!(
                    f,
                    "relation {relation:?} expects {expected} cells, got {got}"
                )
            }
            ErrorKind::State(why) => write!(f, "state: {why}"),
            ErrorKind::Sqlite(why) => write!(f, "sqlite: {why}"),
            ErrorKind::LetRecLimit { rel, limit } => write!(f, "LetRec limit {limit} reached (rel {rel:?})"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<rusqlite::Error> for EngineError {
    fn from(err: rusqlite::Error) -> Self {
        EngineError::new(Stage::Settle, "", ErrorKind::Sqlite(err.to_string()))
    }
}

impl From<EngineError> for rusqlite::Error {
    fn from(err: EngineError) -> Self {
        // The engine rides the vtab/module path, so its errors surface as
        // module errors carrying the full `[stage/relation] kind` text.
        rusqlite::Error::ModuleError(err.to_string())
    }
}
