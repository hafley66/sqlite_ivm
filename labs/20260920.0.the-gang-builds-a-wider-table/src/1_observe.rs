use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tracing::span::Attributes;
use tracing::Id;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

// The filter is a literal in this crate. Nothing about the subscriber stack is
// allowed to depend on the ambient environment, because an unset filter falls
// back to trace upstream and then the logger is the thing being measured.
pub const PINNED_LOG: &str = "warn";

pub fn pin_log_filter() {
    // Unconditional: every entry point re-pins, so an ambient HAFLEY_LOG
    // cannot survive regardless of which call lands first.
    std::env::set_var("HAFLEY_LOG", PINNED_LOG);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanCounts {
    pub instances: BTreeMap<String, usize>,
    pub entries: BTreeMap<String, usize>,
    pub fanout: BTreeMap<(String, String), usize>,
}

// Wall time per span name, accumulated across every entry. This is the
// stopwatch later labs inherit; they must not roll their own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timings {
    pub nanos: BTreeMap<String, u64>,
    pub entries: BTreeMap<String, usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Growth {
    Constant,
    Linear,
    Quadratic,
}

struct Table {
    counts: SpanCounts,
    nanos: BTreeMap<String, u64>,
    names: HashMap<Id, String>,
    opened: HashMap<Id, (String, Instant)>,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            counts: SpanCounts::default(),
            nanos: BTreeMap::default(),
            names: HashMap::default(),
            opened: HashMap::default(),
        }
    }
}

pub struct CountRecorder {
    table: Arc<Mutex<Table>>,
}

pub struct RecorderLayer(Arc<Mutex<Table>>);

impl CountRecorder {
    pub fn new() -> (Self, RecorderLayer) {
        let table = Arc::new(Mutex::new(Table::default()));
        (CountRecorder { table: table.clone() }, RecorderLayer(table))
    }

    pub fn counts(&self) -> SpanCounts {
        self.table.lock().expect("count recorder table").counts.clone()
    }

    pub fn timings(&self) -> Timings {
        let table = self.table.lock().expect("count recorder table");
        Timings {
            nanos: table.nanos.clone(),
            entries: table.counts.entries.clone(),
        }
    }
}

impl Default for CountRecorder {
    fn default() -> Self {
        Self::new().0
    }
}

impl<S> Layer<S> for RecorderLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let name = attrs.metadata().name().to_string();
        let mut table = self.0.lock().expect("count recorder table");
        let parent = attrs
            .parent()
            .cloned()
            .or_else(|| ctx.current_span().id().cloned());
        if let Some(parent) = parent {
            if let Some(parent_name) = table.names.get(&parent).cloned() {
                *table
                    .counts
                    .fanout
                    .entry((parent_name, name.clone()))
                    .or_default() += 1;
            }
        }
        table.names.insert(id.clone(), name.clone());
        *table.counts.instances.entry(name).or_default() += 1;
    }

    fn on_enter(&self, id: &Id, _ctx: Context<'_, S>) {
        let mut table = self.0.lock().expect("count recorder table");
        if let Some(name) = table.names.get(id).cloned() {
            *table.counts.entries.entry(name.clone()).or_default() += 1;
            table.opened.insert(id.clone(), (name, Instant::now()));
        }
    }

    fn on_exit(&self, id: &Id, _ctx: Context<'_, S>) {
        let mut table = self.0.lock().expect("count recorder table");
        if let Some((name, started)) = table.opened.remove(id) {
            *table.nanos.entry(name).or_default() += started.elapsed().as_nanos() as u64;
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let mut table = self.0.lock().expect("count recorder table");
        table.names.remove(&id);
        table.opened.remove(&id);
    }
}

// Installs the recorder for one thread and uninstalls it when the guard drops.
// Thread-local scope keeps parallel tests from counting into each other.
pub fn scoped_subscriber(layer: RecorderLayer) -> tracing::subscriber::DefaultGuard {
    tracing::subscriber::set_default(tracing_subscriber::registry().with(layer))
}

impl SpanCounts {
    pub fn instances_of(&self, name: &str) -> usize {
        self.instances.get(name).copied().unwrap_or_default()
    }

    pub fn entries_of(&self, name: &str) -> usize {
        self.entries.get(name).copied().unwrap_or_default()
    }

    pub fn children_of(&self, parent: &str, child: &str) -> usize {
        self.fanout
            .get(&(parent.to_string(), child.to_string()))
            .copied()
            .unwrap_or_default()
    }

    pub fn assert_instances(&self, name: &str, expected: usize) {
        let actual = self.instances_of(name);
        assert_eq!(actual, expected, "span {name} instance count");
    }

    pub fn assert_children_at_most(&self, parent: &str, child: &str, ceiling: usize) {
        let actual = self.children_of(parent, child);
        assert!(
            actual <= ceiling,
            "fanout {parent} -> {child} was {actual}, ceiling {ceiling}"
        );
    }
}

// The observed class of `name` between two runs whose input sizes differ by
// `size_ratio`. Counts are deterministic, so no tolerance band is needed
// beyond the midpoints that separate the three classes.
pub fn observed_growth(small: &SpanCounts, large: &SpanCounts, name: &str, size_ratio: f64) -> Growth {
    let before = small.entries_of(name) as f64;
    let after = large.entries_of(name) as f64;
    if before <= 0.0 {
        return Growth::Constant;
    }
    let observed = after / before;
    let linear_floor = (1.0 + size_ratio) / 2.0;
    let quadratic_floor = (size_ratio + size_ratio * size_ratio) / 2.0;
    if observed >= quadratic_floor {
        Growth::Quadratic
    } else if observed >= linear_floor {
        Growth::Linear
    } else {
        Growth::Constant
    }
}

pub fn assert_growth(small: &SpanCounts, large: &SpanCounts, name: &str, size_ratio: f64, expected: Growth) {
    let actual = observed_growth(small, large, name, size_ratio);
    assert_eq!(
        actual, expected,
        "span {name} grew {actual:?} from {} to {} entries across a {size_ratio}x input",
        small.entries_of(name),
        large.entries_of(name)
    );
}
