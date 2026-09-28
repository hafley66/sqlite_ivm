//! Argument and result values of the string ops; the ops are in the generated `3_str_ops.rs`.

/// What one argument or result slot holds: dictionary text or a raw integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrKind {
    Text,
    Int,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrVal<'a> {
    Text(&'a str),
    Int(i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrOut {
    Text(String),
    Int(i64),
    /// A filter op held; it adds no column and a miss drops the row.
    Holds,
}

pub(crate) fn text<'a>(args: &[StrVal<'a>], at: usize) -> Option<&'a str> {
    match args.get(at)? {
        StrVal::Text(value) => Some(value),
        StrVal::Int(_) => None,
    }
}

pub(crate) fn int(args: &[StrVal], at: usize) -> Option<i64> {
    match args.get(at)? {
        StrVal::Int(value) => Some(*value),
        StrVal::Text(_) => None,
    }
}
