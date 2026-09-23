//! IVM's phase vocabulary over the shared SQLite statement instrumentation.

pub(crate) use sqlite_ext::statements::{
    batch, exec, exec_cached, guard, open, pragma, query, query_cached, query_map, Statement,
    CACHED, FRESH,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Declare,
    Drain,
    Maintain,
    Fixpoint,
    Materialize,
    Teardown,
}

impl Phase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Declare => "declare",
            Self::Drain => "drain",
            Self::Maintain => "maintain",
            Self::Fixpoint => "fixpoint",
            Self::Materialize => "materialize",
            Self::Teardown => "teardown",
        }
    }
}

impl AsRef<str> for Phase {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
