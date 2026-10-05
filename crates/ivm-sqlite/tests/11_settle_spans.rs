//! Settle tracing: statement, fixpoint and round spans, and dictionary counters. Alone in its own
//! binary: tracing caches callsite interest process-wide, so a scoped subscriber here must not race
//! the unsubscribed settles of other tests.

#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_dd::{Engine, Frontier, SourceChange};
use ivm_sqlite::Sqlite;
use std::sync::Arc;

/// Span names opened and `name=value` fields recorded while a recursive program settles.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _: &tracing::span::Id, _: tracing_subscriber::layer::Context<'_, S>) {
        self.0.lock().unwrap().push(attrs.metadata().name().to_owned());
    }
    fn on_record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Fields<'a>(&'a mut Vec<String>);
        impl tracing::field::Visit for Fields<'_> {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) { self.0.push(format!("{}={value}", field.name())); }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        values.record(&mut Fields(&mut self.0.lock().unwrap()));
    }
}

/// Settle opens one `stmt` span per statement, a `frontier_fixpoint` per SCC with its round counts,
/// a `frontier_round` per round, and records dictionary counters on `frontier_settle`.
#[test]
fn settle_spans_statements_fixpoints_rounds_and_dictionary_counts() {
    use tracing_subscriber::layer::SubscriberExt;
    let capture = Capture::default();
    let program = support::program("7_reach");
    let edges = (0..4).map(|n| SourceChange { rel: 0, row: vec![n, n + 1], w: 1 }).collect();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(capture.clone()), || {
        let mut sql = Sqlite::install(&program).unwrap();
        sql.settle(Frontier { changes: edges }).unwrap();
    });
    let seen = capture.0.lock().unwrap();
    let count = |name: &str| seen.iter().filter(|s| s.as_str() == name || s.starts_with(&format!("{name}="))).count();
    let shape = ["frontier_settle", "frontier_fixpoint", "delete_rounds", "insert_rounds", "udf_calls", "term_lookups"]
        .map(|name| (name, count(name)));
    assert_eq!(shape, [("frontier_settle", 1), ("frontier_fixpoint", 1), ("delete_rounds", 1), ("insert_rounds", 1), ("udf_calls", 1), ("term_lookups", 1)]);
    let rounds: Vec<&str> = seen.iter().filter(|s| s.contains("_rounds=")).map(String::as_str).collect();
    // `stmt` includes install: the source DDL runs inside the install savepoint after one
    // `sqlite_master` read (109 before, with per-source EXISTS and PRAGMA reads), plus the
    // shard DDL batch `Engine::install` runs ahead of it. An insert-only frontier has no
    // retraction: the delete phase is skipped (108 statements and 6 rounds with it).
    // A round reads a variable's change from `to_delta`'s row count (89 with an EXISTS per round).
    // The typed engine reads no output delta table back (80 with the read).
    assert_eq!((count("frontier_round"), count("stmt"), rounds), (5, 79, vec!["delete_rounds=0", "insert_rounds=5"]));
}
