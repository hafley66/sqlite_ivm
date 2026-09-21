use hafley_observe::sqlite::SQLITE_TARGET;
use hafley_observe::CountRecorder;
use rusqlite::Connection;
use tracing_subscriber::prelude::*;

fn run(statements_per_kind: &[(&str, usize)]) -> CountRecorder {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch("CREATE TABLE t(v INTEGER)")
            .expect("schema");
        hafley_observe::sqlite::instrument(&connection);
        let _drain = tracing::debug_span!("drain").entered();
        for (kind, count) in statements_per_kind {
            let _node = tracing::debug_span!("node", kind).entered();
            for value in 0..*count {
                connection
                    .execute("INSERT INTO t VALUES(?1)", [value as i64])
                    .expect("insert");
            }
        }
    });
    recorder
}

#[test]
fn statements_group_under_the_enclosing_node_kind() {
    let recorder = run(&[("map", 2), ("group", 5)]);
    let sums = recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "node", "kind", None);
    let map = &sums[&("map".to_string(), String::new())];
    let group = &sums[&("group".to_string(), String::new())];
    assert_eq!(map.events, 2);
    assert_eq!(group.events, 5);
    assert!(group.sum_of("vm_step") > map.sum_of("vm_step"));
    assert!(group.sum_of("vm_step") > 0.0);
    let spans = recorder.span_counts_by_field("node", "kind");
    assert_eq!(spans["map"], 1);
    assert_eq!(spans["group"], 1);
}

#[test]
fn an_event_field_splits_the_group_by_statement_text() {
    let recorder = run(&[("map", 3)]);
    let sums = recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "node", "kind", Some("sql"));
    let key = ("map".to_string(), "INSERT INTO t VALUES(?1)".to_string());
    assert_eq!(sums[&key].events, 3);
}

#[test]
fn events_outside_the_ancestor_group_under_the_empty_key() {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let connection = Connection::open_in_memory().expect("in-memory database");
        hafley_observe::sqlite::instrument(&connection);
        connection.execute_batch("SELECT 1").expect("select");
    });
    let sums = recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "node", "kind", None);
    assert_eq!(sums[&(String::new(), String::new())].events, 1);
}
