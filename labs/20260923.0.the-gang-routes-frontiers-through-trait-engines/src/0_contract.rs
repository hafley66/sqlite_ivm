use std::fmt;

/// Integer source IDs are stable across an installed program. Rows carry a
/// separate integer identity, so equal values from two rows retain support.
pub type SourceId = u8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub id: i64,
    pub cells: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub source: SourceId,
    pub row: SourceRow,
    pub weight: i8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightedRow {
    pub cells: Vec<i64>,
    pub weight: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontier {
    pub id: u64,
    pub changes: Vec<WeightedRow>,
}

/// The two graph shapes in the common ISO packet. Columns are zero-based
/// positions in source rows, excluding their integer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    JoinUnion {
        left: SourceId,
        right: SourceId,
        direct: SourceId,
        left_key: usize,
        right_key: usize,
        left_output: usize,
        right_output: usize,
        direct_output: [usize; 2],
    },
    GroupCountSum {
        source: SourceId,
        group: usize,
        value: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub stage: &'static str,
    pub source: Option<SourceId>,
    pub message: String,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:?}: {}", self.stage, self.source, self.message)
    }
}

impl std::error::Error for EngineError {}

/// An installed program owns its state until teardown. One `apply` call is one
/// settled frontier and returns its net output changes. Associated types allow
/// a Rust row store and a SQLite extension to share caller control flow while
/// retaining different plan and error representations.
pub trait FrontierEngine {
    type Plan;
    type Change;
    type Output;
    type Error: std::error::Error;

    fn install(&mut self, plan: Self::Plan) -> Result<(), Self::Error>;
    fn apply(&mut self, changes: &[Self::Change]) -> Result<Frontier, Self::Error>;
    fn snapshot(&self) -> Result<Vec<Self::Output>, Self::Error>;
    fn teardown(&mut self) -> Result<(), Self::Error>;
}
