//! Application-specific observation names.
//!
//! Spans and events under the `frontier` target name this engine's stages;
//! SQLite statement spans and PROFILE counters stay under the `sqlite` target
//! owned by `hafley-observe`. No subscriber is installed here — the host
//! (linked test, example, or the extension's `Plugin`) owns that.

/// The tracing target for this engine's own spans and events.
pub(crate) const TARGET: &str = "frontier";

/// Span opened while a program installs.
pub(crate) const INSTALL_SPAN: &str = "frontier_install";
/// Span opened around one settled frontier.
pub(crate) const SETTLE_SPAN: &str = "frontier_settle";
/// Span opened while a program tears down.
pub(crate) const TEARDOWN_SPAN: &str = "frontier_teardown";
/// One scan's netted delta fill.
pub(crate) const SCAN_SPAN: &str = "frontier_scan";
/// One join's three-term derivation delta.
pub(crate) const JOIN_SPAN: &str = "frontier_join";
/// The root application (union or aggregate).
pub(crate) const ROOT_SPAN: &str = "frontier_root";

/// Emitted once per settled frontier.
pub(crate) const SETTLED_EVENT: &str = "frontier_settled";
