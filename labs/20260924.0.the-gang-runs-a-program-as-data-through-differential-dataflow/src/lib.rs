pub mod ir {
    pub use ivm_ir::*;
}
pub mod rel {
    pub use ivm_engine::*;
}
#[path = "2_dd.rs"]
pub mod dd;

pub use dd::{Dd, DdTap};
pub use ir::*;
pub use rel::{eval, lower, lower_node, Engine, EngineError, ErrorKind, Rel, Stage};
#[cfg(feature = "sqlite")]
#[path = "3_sqlite.rs"]
pub mod sqlite;
#[cfg(feature = "sqlite")]
pub use sqlite::{Sql, SqlSeen, SqlTag};
