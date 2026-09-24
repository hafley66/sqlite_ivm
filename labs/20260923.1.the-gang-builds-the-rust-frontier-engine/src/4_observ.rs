//! Observation events for `hafley-observe`. Application-specific names live
//! here, in the implementation crate, per the brief.

/// The crate's tracing target.
pub(crate) const TARGET: &str = "frontier_engine";

/// Span opened for one frontier application. `output_changes` is recorded
/// when the frontier settles.
pub(crate) fn frontier_span(id: &str, input_changes: usize) -> tracing::Span {
    tracing::info_span!(
        target: TARGET,
        "frontier",
        frontier = %id,
        input_changes = input_changes as u64,
        output_changes = tracing::field::Empty,
    )
}

/// Span for maintaining one plan node in one frontier.
pub(crate) fn maintain_span(node: u32, kind: &'static str, delta_rows: usize) -> tracing::Span {
    tracing::info_span!(
        target: TARGET,
        "maintain",
        node = node,
        kind = kind,
        delta_rows = delta_rows as u64,
    )
}

/// Event for one program installation with its guardrail counts.
pub(crate) fn emit_install(operators: usize, arrangements: usize, outputs: usize) {
    tracing::info!(
        target: TARGET,
        operators = operators as u64,
        arrangements = arrangements as u64,
        outputs = outputs as u64,
        "install"
    );
}
