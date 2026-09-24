//! Randomized differential harness: generated programs and frontiers, SQLite recompute oracle, metamorphic checks.

#[path = "0_rng.rs"]
pub mod rng;
#[path = "1_gen.rs"]
pub mod gen;
#[path = "2_sql.rs"]
pub mod sql;
#[path = "3_drive.rs"]
pub mod drive;
#[path = "4_meta.rs"]
pub mod meta;

pub use drive::run;
