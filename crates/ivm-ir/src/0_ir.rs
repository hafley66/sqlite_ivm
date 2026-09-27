//! The program as data; nothing here names DD or SQLite.
//! Op names follow Materialize MIR (plans/2026-09-24-ivm-cousins/4_design.md §2.1).

use serde::{Deserialize, Serialize};

pub type RelId = u32;
pub type NodeId = u32;
pub type ColId = u16;

/// One cell. Int passes through; Text is interned to an i64 id before it reaches an engine.
pub type Cell = i64;
pub type Row = Vec<Cell>;
/// Z-set weight.
pub type W = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ty {
    Int,
    Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelKind {
    Source,
    Derived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub id: RelId,
    pub name: String,
    pub cols: Vec<Ty>,
    pub kind: RelKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Func {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    And,
    Or,
    Not,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expr {
    Col(ColId),
    Lit(Cell),
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Agg {
    Count,
    Sum(ColId),
    Min(ColId),
    Max(ColId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub col: ColId,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WinFn {
    RowNumber,
    Rank,
    DenseRank,
    Lag(u32),
    Lead(u32),
    Sum(ColId),
    Count,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Get(RelId),
    /// Filter, then append `map` columns, then keep `project` (empty = keep all).
    Mfp {
        input: NodeId,
        #[serde(default)]
        filter: Vec<Expr>,
        #[serde(default)]
        map: Vec<Expr>,
        #[serde(default)]
        project: Vec<ColId>,
    },
    Union(Vec<NodeId>),
    Negate(NodeId),
    /// Output columns are the inputs' columns concatenated. Each equivalence class lists
    /// `(input position, column)` pairs that must be equal.
    Join {
        inputs: Vec<NodeId>,
        equivalences: Vec<Vec<(u8, ColId)>>,
    },
    Antijoin {
        l: NodeId,
        r: NodeId,
        lk: Vec<ColId>,
        rk: Vec<ColId>,
    },
    /// Output columns: `key` columns, then one per aggregate.
    Reduce {
        input: NodeId,
        key: Vec<ColId>,
        aggs: Vec<Agg>,
    },
    /// Set semantics: accumulated weight > 0 becomes 1, otherwise absent.
    Threshold(NodeId),
    TopK {
        input: NodeId,
        key: Vec<ColId>,
        order: Vec<Order>,
        limit: u32,
    },
    Window {
        input: NodeId,
        partition: Vec<ColId>,
        order: Vec<Order>,
        func: WinFn,
    },
    Delay(NodeId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetRec {
    pub ids: Vec<RelId>,
    pub bodies: Vec<NodeId>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stratum {
    Let { id: RelId, body: NodeId },
    LetRec(LetRec),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    pub rels: Vec<Relation>,
    pub nodes: Vec<Op>,
    pub strata: Vec<Stratum>,
    pub outputs: Vec<RelId>,
}

impl Program {
    pub fn rel(&self, id: RelId) -> Option<&Relation> {
        self.rels.iter().find(|rel| rel.id == id)
    }
}

/// One signed change to one source row. `w` is +1 or -1 at the raw API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChange {
    pub rel: RelId,
    pub row: Row,
    pub w: W,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    pub changes: Vec<SourceChange>,
}

/// Net signed output changes of one settled frontier, sorted by `(rel, row)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub tick: u64,
    pub changes: Vec<(RelId, Row, W)>,
}
