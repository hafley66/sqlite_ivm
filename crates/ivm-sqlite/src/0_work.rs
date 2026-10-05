//! This engine's view of `hafley_observe::sqlite_work`: rows written folded by table kind, and one
//! `ivm_sqlite::work` info event per reported interval.

use hafley_observe::sqlite_work::{self, WorkTrace};
use sqlite_ext::rusqlite::Connection;

/// `sqlite_work::Work` with `written` folded by table kind; field docs on that type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub creates: u64,
    pub schema_rows_parsed: u64,
    pub prepared: u64,
    pub prepared_bytes: u64,
    pub unfolded_bytes: u64,
    pub executions: u64,
    pub zero_row_executions: u64,
    pub vm_steps: u64,
    pub fullscan_steps: u64,
    pub sorts: u64,
    pub autoindexes: u64,
    pub reprepares: u64,
    pub rows_returned: u64,
    pub written_d: u64,
    pub written_i: u64,
    pub written_x: u64,
    pub written_scratch: u64,
    pub written_source: u64,
    pub written_catalog: u64,
    pub written_term: u64,
}

impl From<&sqlite_work::Work> for Work {
    fn from(work: &sqlite_work::Work) -> Work {
        let written = |of: Kind| work.written_where(|table| kind(table) == of);
        Work {
            creates: work.creates,
            schema_rows_parsed: work.schema_rows_parsed,
            prepared: work.prepared,
            prepared_bytes: work.prepared_bytes,
            unfolded_bytes: work.unfolded_bytes,
            executions: work.executions,
            zero_row_executions: work.zero_row_executions,
            vm_steps: work.vm_steps,
            fullscan_steps: work.fullscan_steps,
            sorts: work.sorts,
            autoindexes: work.autoindexes,
            reprepares: work.reprepares,
            rows_returned: work.rows_returned,
            written_d: written(Kind::D),
            written_i: written(Kind::I),
            written_x: written(Kind::X),
            written_scratch: written(Kind::Scratch),
            written_source: written(Kind::Source),
            written_catalog: written(Kind::Catalog),
            written_term: written(Kind::Term),
        }
    }
}

impl Work {
    pub fn since(&self, earlier: &Work) -> Work {
        let (a, b) = (self.fields(), earlier.fields());
        Work::from_fields(std::array::from_fn(|i| a[i] - b[i]))
    }

    pub fn add(&mut self, other: &Work) {
        let (a, b) = (self.fields(), other.fields());
        *self = Work::from_fields(std::array::from_fn(|i| a[i].saturating_add(b[i])));
    }

    fn fields(&self) -> [u64; 20] {
        [self.creates, self.schema_rows_parsed, self.prepared, self.prepared_bytes, self.unfolded_bytes,
         self.executions, self.zero_row_executions, self.vm_steps, self.fullscan_steps, self.sorts,
         self.autoindexes, self.reprepares, self.rows_returned, self.written_d, self.written_i,
         self.written_x, self.written_scratch, self.written_source, self.written_catalog, self.written_term]
    }

    fn from_fields(f: [u64; 20]) -> Work {
        Work {
            creates: f[0], schema_rows_parsed: f[1], prepared: f[2], prepared_bytes: f[3], unfolded_bytes: f[4],
            executions: f[5], zero_row_executions: f[6], vm_steps: f[7], fullscan_steps: f[8], sorts: f[9],
            autoindexes: f[10], reprepares: f[11], rows_returned: f[12], written_d: f[13], written_i: f[14],
            written_x: f[15], written_scratch: f[16], written_source: f[17], written_catalog: f[18], written_term: f[19],
        }
    }

    /// One `ivm_sqlite::work` info event; `phase` `between` is work outside install and settle.
    pub fn log(&self, phase: &'static str) {
        tracing::info!(target: "ivm_sqlite::work", phase,
            creates = self.creates, schema_rows_parsed = self.schema_rows_parsed,
            prepared = self.prepared, prepared_bytes = self.prepared_bytes, unfolded_bytes = self.unfolded_bytes,
            executions = self.executions, zero_row_executions = self.zero_row_executions,
            vm_steps = self.vm_steps, fullscan_steps = self.fullscan_steps, sorts = self.sorts,
            autoindexes = self.autoindexes, reprepares = self.reprepares, rows_returned = self.rows_returned,
            written_d = self.written_d, written_i = self.written_i, written_x = self.written_x,
            written_scratch = self.written_scratch, written_source = self.written_source,
            written_catalog = self.written_catalog, written_term = self.written_term,
            "sqlite work");
    }
}

/// Counting on one connection; dropping it reports the rest as `between`, and must precede the
/// connection's close.
pub struct WorkLog {
    trace: WorkTrace,
}

impl WorkLog {
    /// Whether `Engine::install` counts: the `ivm_sqlite::work` target is enabled at info.
    pub fn wanted() -> bool {
        tracing::enabled!(target: "ivm_sqlite::work", tracing::Level::INFO)
    }

    pub fn start(db: &Connection) -> sqlite_ext::rusqlite::Result<WorkLog> {
        Ok(WorkLog { trace: WorkTrace::start(db)? })
    }

    pub fn work(&self) -> Work {
        Work::from(&self.trace.work())
    }

    /// Logs the counts since the last report under `phase`.
    pub fn report(&mut self, phase: &'static str) {
        let interval = Work::from(&self.trace.interval());
        if interval != Work::default() { interval.log(phase); }
    }
}

impl Drop for WorkLog {
    fn drop(&mut self) {
        self.report("between");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind { D, I, X, Scratch, Source, Catalog, Term }

fn kind(table: &str) -> Kind {
    const TERM: [&str; 6] = ["ivm_term", "ivm_text", "ivm_functor", "ivm_functor_col", "ivm_cell_dict", "ivm_blob_dict"];
    if table.starts_with("ivm_ctor_") || TERM.contains(&table) { return Kind::Term; }
    if table.starts_with("frontier_catalog") || table == "frontier_dependency" || table == "frontier_install" || table.starts_with("sqlite_") {
        return Kind::Catalog;
    }
    let mut parts = table.rsplit('_');
    let (last, node) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let is_node = node.len() > 1 && node.starts_with('n') && node[1..].bytes().all(|b| b.is_ascii_digit());
    if is_node {
        return match last {
            "d" => Kind::D,
            "i" => Kind::I,
            x if x.starts_with('x') && x[1..].bytes().all(|b| b.is_ascii_digit()) => Kind::X,
            _ => Kind::Scratch,
        };
    }
    if table.starts_with("frontier_") || table.starts_with("ivm_") { Kind::Scratch } else { Kind::Source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tables() {
        let kinds = ["frontier_p_n12_d", "ivm_n4_i", "ivm_n4_x2", "ivm_n4_nx", "frontier_p_stage", "edges", "frontier_catalog", "ivm_ctor_ab"].map(kind);
        assert_eq!(kinds, [Kind::D, Kind::I, Kind::X, Kind::Scratch, Kind::Scratch, Kind::Source, Kind::Catalog, Kind::Term]);
    }
}
