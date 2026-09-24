//! An in-process incremental relational engine over typed integer rows.
//!
//! The public door is [`FrontierEngine`]: install a [`Program`], feed it
//! atomic [`Frontier`]s of signed [`SourceChange`]s, read snapshots and net
//! signed [`OutputDelta`]s. [`Engine`] adds relation definition and
//! transaction/savepoint scopes. Both packet cases (the grant union-join and
//! the grouped COUNT/SUM) run through the same signatures.

#[path = "2_apply.rs"]
mod apply;
#[path = "4_observ.rs"]
mod observ;
#[path = "1_storage.rs"]
mod storage;
#[path = "3_tx.rs"]
mod tx;
#[path = "0_types.rs"]
pub mod types;

pub use storage::Engine;
pub use tx::{Savepoint, Transaction};
pub use types::{
    Cell, EngineError, EngineStats, ErrorKind, Frontier, FrontierEngine, OutputChange, OutputDelta,
    PlanNode, Program, ProgramId, RelId, Row, RowId, Sign, SourceChange, Stage,
};
