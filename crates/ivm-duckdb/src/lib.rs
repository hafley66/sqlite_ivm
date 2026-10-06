//! External DuckDB CLI engine. SQL is the only connection to DuckDB.
#[path = "2_engine.rs"]
mod engine;
#[path = "1_sql.rs"]
mod sql;
#[path = "0_transport.rs"]
mod transport;
pub use engine::DuckDb;
pub use ivm_engine::*;
pub use ivm_ir::*;
pub use transport::Work;
