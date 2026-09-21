use tracing_capture::{CaptureLayer, SharedStorage};
use tracing_subscriber::prelude::*;

#[test]
fn nested_span_counts_link_children_to_parents() {
    let storage = SharedStorage::default();
    let subscriber = tracing_subscriber::registry().with(CaptureLayer::new(&storage));

    tracing::subscriber::with_default(subscriber, || {
        let populate = tracing::debug_span!("populate", rows = 3);
        let _populate_guard = populate.enter();
        for row in 0..3 {
            let maintain = tracing::debug_span!("maintain", row = row);
            let _maintain_guard = maintain.enter();
        }
    });

    let storage = storage.lock();
    let populate_spans: Vec<_> = storage
        .all_spans()
        .filter(|span| span.metadata().name() == "populate")
        .collect();
    assert_eq!(populate_spans.len(), 1, "exactly one populate instance expected");
    let populate = &populate_spans[0];
    assert_eq!(populate.stats().entered, 1);

    let maintain_children: Vec<_> = populate
        .children()
        .filter(|span| span.metadata().name() == "maintain")
        .collect();
    assert_eq!(maintain_children.len(), 3, "child count under populate must equal rows");
    for child in &maintain_children {
        assert_eq!(child.stats().entered, 1);
        let parent = child.parent().expect("maintain must link to populate");
        assert_eq!(parent.metadata().name(), "populate");
    }

    let total_maintains = storage
        .all_spans()
        .filter(|span| span.metadata().name() == "maintain")
        .count();
    assert_eq!(total_maintains, 3);
}
