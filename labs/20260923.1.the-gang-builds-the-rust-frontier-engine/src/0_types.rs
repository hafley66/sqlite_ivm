//! Public contract of the in-process frontier engine.
//!
//! Everything a caller crate needs is here: the typed row domain, the plan
//! language, the frontier wire types, the [`FrontierEngine`] trait, and the
//! error type that names the failing stage and relation.

use std::fmt;
/// A typed cell. Every packet relation is first-normal-form integers, so the
/// domain is fixed: no generic value parameter that only renames `i64`.
pub type Cell = i64;

/// One row of typed cells. Arity is fixed per relation and checked at
/// [`Engine::define_relation`] and at install.
pub type Row = Vec<Cell>;

/// Identity of a source relation inside one [`Engine`]. Arena index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RelId(pub u32);

/// Identity of one source row. The arena slot of the row, distinct from value
/// equality: two rows with equal cells keep separate ids.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RowId(pub u32);

/// Identity of an installed program; the arena index of its plan nodes and
/// arrangements ([`EngineStats`]). Concrete `u32` here; an alternate engine
/// with durable storage would substitute durable ids — that is the seam the
/// [`FrontierEngine::ProgramId`] associated type preserves.
pub type ProgramId = u32;

/// Direction of one signed change.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Sign {
    /// Row appears.
    Plus,
    /// Row disappears.
    Minus,
}

impl Sign {
    /// The multiplicity this sign contributes.
    pub fn weight(self) -> i64 {
        match self {
            Sign::Plus => 1,
            Sign::Minus => -1,
        }
    }
}

impl fmt::Display for Sign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sign::Plus => f.write_str("+"),
            Sign::Minus => f.write_str("-"),
        }
    }
}

/// One signed source change inside a frontier.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceChange {
    /// Relation the row belongs to.
    pub relation: RelId,
    /// Direction of the change.
    pub sign: Sign,
    /// Typed cells of the row.
    pub row: Row,
}

/// One atomic frontier: a complete batch of signed source changes. All writes
/// in the batch settle before its output is observed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frontier {
    /// Label carried into observations, e.g. the oracle frontier name.
    pub id: String,
    /// The signed source changes, in caller order; consolidation is the
    /// engine's job.
    pub changes: Vec<SourceChange>,
}

impl Frontier {
    /// A frontier with no changes; exercises the no-output path.
    pub fn empty(id: impl Into<String>) -> Self {
        Frontier {
            id: id.into(),
            changes: Vec::new(),
        }
    }
}

/// One signed output row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OutputChange {
    /// Direction of the change.
    pub sign: Sign,
    /// Typed cells of the output row.
    pub row: Row,
}

/// Net signed output change of one named output for one frontier.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OutputDelta {
    /// Output name, as declared in the [`Program`].
    pub output: String,
    /// Net signed rows, sorted by row cells ascending.
    pub changes: Vec<OutputChange>,
}

/// Plan language. `Scan`, `Union`, `Join`, and `Aggregate` are maintained
/// incrementally; [`PlanNode::Difference`] exists so a caller can probe the
/// unsupported path and receive [`ErrorKind::Unsupported`] instead of a
/// success-shaped recomputation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PlanNode {
    /// All rows of one source relation.
    Scan {
        /// Relation to scan.
        relation: RelId,
    },
    /// Multiset sum of the inputs; a set output deduplicates by support count.
    Union {
        /// Input subplans; arities must agree.
        inputs: Vec<PlanNode>,
    },
    /// Equi-join of two subplans on one column pair per side.
    Join {
        /// Left subplan.
        left: Box<PlanNode>,
        /// Right subplan.
        right: Box<PlanNode>,
        /// Column of the left output that carries the key.
        on_left: usize,
        /// Column of the right output that carries the key.
        on_right: usize,
    },
    /// Grouped COUNT(*) and/or SUM(column) over one subplan. The output row is
    /// the group key cells, then the count if requested, then the sum if
    /// requested. Requires a non-empty key and at least one function.
    Aggregate {
        /// Input subplan.
        input: Box<PlanNode>,
        /// Columns of the input that form the group key.
        group_by: Vec<usize>,
        /// Emit COUNT(*) after the key cells.
        count: bool,
        /// Emit SUM of this input column after the count.
        sum: Option<usize>,
    },
    /// Column subset of one subplan, preserving multiplicity. The packet's
    /// join arm projects the joined pair down to (person, resource).
    Project {
        /// Input subplan.
        input: Box<PlanNode>,
        /// Columns of the input, in output order; repetition allowed.
        columns: Vec<usize>,
    },
    /// Relational difference. Explicitly unsupported: install returns
    /// [`ErrorKind::Unsupported`] naming the shape.
    Difference {
        /// Rows kept unless present in this subplan.
        minuend: Box<PlanNode>,
        /// Rows subtracted from the minuend.
        subtrahend: Box<PlanNode>,
    },
}

/// One named output of a [`Program`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OutputDecl {
    /// Caller-facing name; `snapshot` and [`OutputDelta::output`] use it.
    pub name: String,
    /// The plan that produces the output.
    pub plan: PlanNode,
}

/// An installable program: named outputs over shared source relations.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Program {
    /// The named outputs, in declaration order.
    pub outputs: Vec<OutputDecl>,
}

impl Program {
    /// A program with one output.
    pub fn one(name: impl Into<String>, plan: PlanNode) -> Self {
        Program {
            outputs: vec![OutputDecl {
                name: name.into(),
                plan,
            }],
        }
    }
}

/// Which stage of a call failed. Part of every [`EngineError`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Front-of-`apply` frontier validation, before any write.
    Validate,
    /// Program install/teardown.
    Install,
    /// Source-table writes inside a frontier.
    Source,
    /// Incremental propagation through plan nodes.
    Maintain,
    /// Snapshot read.
    Snapshot,
    /// Transaction scope misuse.
    Transaction,
}

/// What went wrong. Every variant that involves a relation or output names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// The relation id is not defined on this engine.
    UnknownRelation(RelId),
    /// Row arity does not match the relation or the plan node.
    ArityMismatch {
        /// Arity registered for the relation or expected by the node.
        expected: usize,
        /// Arity of the offending row or subplan.
        actual: usize,
    },
    /// The output name is not declared by the installed program.
    UnknownOutput(String),
    /// The program id is not installed (or was uninstalled).
    UnknownProgram(ProgramId),
    /// A required plan shape is not implemented. No success-shaped fallback to
    /// full recomputation exists; the caller sees this variant instead.
    Unsupported {
        /// The missing shape, e.g. `"difference"`.
        shape: &'static str,
    },
    /// A maintained invariant broke (negative support, bad undo replay). The
    /// frontier is unwound; the previous committed state stays readable.
    Internal(String),
    /// Transaction scope misuse (commit of an already-settled scope).
    Scope(String),
}

/// The error result of the engine: names the [`Stage`] and, when one is
/// involved, the relation or output.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EngineError {
    /// Stage that failed.
    pub stage: Stage,
    /// What failed at that stage.
    pub kind: ErrorKind,
    /// Resolved relation name, when the failure involves one relation.
    pub relation: Option<String>,
    /// Resolved output name, when the failure involves one output.
    pub output: Option<String>,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} stage", self.stage)?;
        if let Some(relation) = &self.relation {
            write!(f, ", relation {relation}")?;
        }
        if let Some(output) = &self.output {
            write!(f, ", output {output}")?;
        }
        match &self.kind {
            ErrorKind::UnknownRelation(id) => write!(f, ": unknown relation {id:?}"),
            ErrorKind::ArityMismatch { expected, actual } => {
                write!(f, ": arity {actual}, expected {expected}")
            }
            ErrorKind::UnknownOutput(name) => write!(f, ": unknown output {name:?}"),
            ErrorKind::UnknownProgram(id) => write!(f, ": unknown program {id}"),
            ErrorKind::Unsupported { shape } => write!(f, ": unsupported shape {shape:?}"),
            ErrorKind::Internal(detail) => write!(f, ": internal {detail}"),
            ErrorKind::Scope(detail) => write!(f, ": {detail}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Contractual guardrail counts. These are exact object counts the engine
/// keeps by construction; timings live in observations, not here.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EngineStats {
    /// Programs currently installed.
    pub installed_programs: usize,
    /// Plan nodes across installed programs.
    pub operators: usize,
    /// Maintained multisets and indexes across installed programs (count maps,
    /// join key indexes, aggregate group maps).
    pub arrangements: usize,
    /// Live source row identities across all relations.
    pub source_rows: usize,
}

/// The public seam of the engine. Both packet cases run through these five
/// methods with unchanged signatures; the concrete [`Engine`] adds relation
/// definition and transaction scopes.
///
/// Associated types, per the brief's "only where they allow a real alternate
/// implementation" rule:
/// - `ProgramId` — this engine uses arena indexes; a durable engine substitutes
///   durable keys. Call sites: [`FrontierEngine::apply`], [`FrontierEngine::snapshot`],
///   [`FrontierEngine::uninstall`] — the id must stay valid between install and
///   uninstall, which is the compile-time invariant `Copy + Eq` here.
/// - `Error` — a host embedding this engine may map failures into its own error
///   type. Call sites: every fallible method of this trait.
///
/// There is deliberately no associated row/value type: the packet fixes the
/// domain to typed integer cells, so such a parameter would only rename `i64`.
pub trait FrontierEngine {
    /// Identity of one installed program.
    type ProgramId: Copy + Eq + std::hash::Hash + std::fmt::Debug;
    /// Failure carrying the stage and the relation/output involved.
    type Error: std::fmt::Debug + std::fmt::Display;

    /// Install a program: validate every output plan and materialize its
    /// arrangements. Installation is separate from source-change application.
    fn install(&mut self, program: &Program) -> Result<Self::ProgramId, Self::Error>;

    /// Remove a program and drop its arrangements. Ids stay invalid after this.
    fn uninstall(&mut self, program: Self::ProgramId) -> Result<(), Self::Error>;

    /// Apply one atomic frontier and return only after it is settled: the
    /// returned deltas are the net signed output changes and the snapshot is
    /// readable immediately. On [`Err`], the frontier is unwound and the
    /// previous committed state stays readable.
    fn apply(
        &mut self,
        program: Self::ProgramId,
        frontier: Frontier,
    ) -> Result<Vec<OutputDelta>, Self::Error>;

    /// Visible rows of one output: support greater than zero for set outputs,
    /// live groups for aggregate outputs, sorted by row cells ascending.
    fn snapshot(&self, program: Self::ProgramId, output: &str) -> Result<Vec<Row>, Self::Error>;

    /// Contractual guardrail counts for the whole engine.
    fn stats(&self) -> EngineStats;
}
