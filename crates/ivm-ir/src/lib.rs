#[path = "0_ir.rs"]
mod ir;
#[path = "1_terms.rs"]
mod terms;
#[path = "2_strings.rs"]
mod strings;
#[path = "3_str_ops.rs"]
mod str_ops;
#[path = "4_compose.rs"]
mod compose;

pub use ir::*;
pub use terms::*;
pub use strings::{StrKind, StrOut, StrVal};
pub(crate) use strings::{int, text};
pub use str_ops::StrOp;
pub use compose::{compose, Composed};
