use std::collections::BTreeMap;

use tracing_capture::{CaptureLayer, SharedStorage};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanCounts {
    pub instances: BTreeMap<String, usize>,
    pub entries: BTreeMap<String, usize>,
    pub fanout: BTreeMap<(String, String), usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Growth {
    Constant,
    Linear,
    Quadratic,
}

pub struct CountRecorder {
    storage: SharedStorage,
}

impl CountRecorder {
    pub fn new() -> (Self, CaptureLayer<tracing_subscriber::Registry>) {
        let storage = SharedStorage::default();
        let layer = CaptureLayer::new(&storage);
        (CountRecorder { storage }, layer)
    }

    pub fn counts(&self) -> SpanCounts {
        let storage = self.storage.lock();
        let mut counts = SpanCounts::default();
        for span in storage.all_spans() {
            let name = span.metadata().name().to_string();
            *counts.instances.entry(name.clone()).or_default() += 1;
            *counts.entries.entry(name.clone()).or_default() += span.stats().entered;
            if let Some(parent) = span.parent() {
                let edge = (parent.metadata().name().to_string(), name);
                *counts.fanout.entry(edge).or_default() += 1;
            }
        }
        counts
    }
}

/// Events under one ancestor span, numeric fields summed. `events` counts them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventSums {
    pub events: usize,
    pub sums: BTreeMap<String, f64>,
}

impl EventSums {
    pub fn sum_of(&self, field: &str) -> f64 {
        self.sums.get(field).copied().unwrap_or_default()
    }
}

impl CountRecorder {
    /// Group key is (`ancestor_field` on the nearest `ancestor` span, `event_field`
    /// on the event); either side is empty when absent.
    pub fn event_sums(
        &self,
        target: &str,
        level: tracing::Level,
        ancestor: &str,
        ancestor_field: &str,
        event_field: Option<&str>,
    ) -> BTreeMap<(String, String), EventSums> {
        let storage = self.storage.lock();
        let mut groups: BTreeMap<(String, String), EventSums> = BTreeMap::new();
        for event in storage.all_events() {
            if event.metadata().target() != target || *event.metadata().level() != level {
                continue;
            }
            let group = event
                .ancestors()
                .find(|span| span.metadata().name() == ancestor)
                .and_then(|span| span.value(ancestor_field).map(text))
                .unwrap_or_default();
            let key = event_field
                .and_then(|name| event.value(name).map(text))
                .unwrap_or_default();
            let sums = groups.entry((group, key)).or_default();
            sums.events += 1;
            for (name, value) in event.values() {
                let number = value
                    .as_float()
                    .or_else(|| value.as_int().map(|n| n as f64))
                    .or_else(|| value.as_uint().map(|n| n as f64));
                if let Some(number) = number {
                    *sums.sums.entry(name.to_string()).or_default() += number;
                }
            }
        }
        groups
    }
}

impl CountRecorder {
    /// Instances of span `name`, grouped by the text of its `field`.
    pub fn span_counts_by_field(&self, name: &str, field: &str) -> BTreeMap<String, usize> {
        let storage = self.storage.lock();
        let mut counts = BTreeMap::new();
        for span in storage.all_spans() {
            if span.metadata().name() != name {
                continue;
            }
            let value = span.value(field).map(text).unwrap_or_default();
            *counts.entry(value).or_default() += 1;
        }
        counts
    }
}

fn text(value: &tracing_tunnel::TracedValue) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_debug_str().map(str::to_string))
        .unwrap_or_else(|| format!("{value:?}"))
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

/// The observed class of `name` between two runs whose input sizes differ by
/// `size_ratio`. Counts are deterministic, so no tolerance band is needed
/// beyond the midpoints that separate the three classes.
pub fn observed_growth(
    small: &SpanCounts,
    large: &SpanCounts,
    name: &str,
    size_ratio: f64,
) -> Growth {
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

pub fn assert_growth(
    small: &SpanCounts,
    large: &SpanCounts,
    name: &str,
    size_ratio: f64,
    expected: Growth,
) {
    let actual = observed_growth(small, large, name, size_ratio);
    assert_eq!(
        actual,
        expected,
        "span {name} grew {:?} from {} to {} entries across a {size_ratio}x input",
        actual,
        small.entries_of(name),
        large.entries_of(name)
    );
}

/// Numeric samples retained for exact sums and nearest-rank percentiles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldStats {
    pub samples: Vec<f64>,
}

impl FieldStats {
    pub fn sum(&self) -> f64 {
        self.samples.iter().sum()
    }

    pub fn mean(&self) -> Option<f64> {
        (!self.samples.is_empty()).then(|| self.sum() / self.samples.len() as f64)
    }

    /// Nearest-rank percentile in [0, 100]. Empty samples return None.
    pub fn percentile(&self, percentile: f64) -> Option<f64> {
        assert!((0.0..=100.0).contains(&percentile));
        if self.samples.is_empty() {
            return None;
        }
        let mut samples = self.samples.clone();
        samples.sort_by(f64::total_cmp);
        let index = ((samples.len() as f64 * percentile / 100.0).ceil() as usize).saturating_sub(1);
        Some(samples[index])
    }
}

/// Numeric event fields and nearest-ancestor fields, sampled once per event.
/// Missing fields have no sample. Ancestor fields reflect their final recorded
/// value, including fields recorded after an event was emitted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventStats {
    pub events: usize,
    pub fields: BTreeMap<String, FieldStats>,
    pub ancestor_fields: BTreeMap<String, FieldStats>,
}

impl CountRecorder {
    /// Group matching events by caller-selected fields of their nearest named
    /// ancestor. Events without that ancestor are excluded. Missing grouping
    /// fields use the empty string, matching `event_sums`.
    pub fn event_stats<const N: usize>(
        &self,
        target: &str,
        level: tracing::Level,
        ancestor: &str,
        group_fields: [&str; N],
    ) -> BTreeMap<[String; N], EventStats> {
        let storage = self.storage.lock();
        let mut groups = BTreeMap::<[String; N], EventStats>::new();
        for event in storage.all_events() {
            if event.metadata().target() != target || *event.metadata().level() != level {
                continue;
            }
            let Some(span) = event
                .ancestors()
                .find(|span| span.metadata().name() == ancestor)
            else {
                continue;
            };
            let key = group_fields.map(|field| span.value(field).map(text).unwrap_or_default());
            let stats = groups.entry(key).or_default();
            stats.events += 1;
            for (name, value) in event.values() {
                if let Some(number) = number(value) {
                    stats
                        .fields
                        .entry(name.to_string())
                        .or_default()
                        .samples
                        .push(number);
                }
            }
            for (name, value) in span.values() {
                if let Some(number) = number(value) {
                    stats
                        .ancestor_fields
                        .entry(name.to_string())
                        .or_default()
                        .samples
                        .push(number);
                }
            }
        }
        groups
    }
}

fn number(value: &tracing_tunnel::TracedValue) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_int().map(|value| value as f64))
        .or_else(|| value.as_uint().map(|value| value as f64))
}
