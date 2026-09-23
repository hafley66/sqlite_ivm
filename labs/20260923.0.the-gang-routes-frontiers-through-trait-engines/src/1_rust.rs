use crate::{Change, EngineError, Frontier, FrontierEngine, Plan, WeightedRow};
use std::collections::{BTreeMap, BTreeSet, HashMap};

type Rows = HashMap<i64, Vec<i64>>;
type Pending = BTreeMap<(u8, i64), (Option<Vec<i64>>, Option<Vec<i64>>)>;

#[derive(Default)]
pub struct RustEngine {
    plan: Option<Plan>,
    rows: HashMap<u8, Rows>,
    indexes: HashMap<u8, HashMap<i64, BTreeSet<i64>>>,
    support: BTreeMap<Vec<i64>, i64>,
    groups: BTreeMap<i64, (i64, i64)>,
    next_frontier: u64,
}

impl RustEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn error(stage: &'static str, source: Option<u8>, message: impl Into<String>) -> EngineError {
        EngineError {
            stage,
            source,
            message: message.into(),
        }
    }

    fn key_column(plan: &Plan, source: u8) -> Option<usize> {
        match plan {
            Plan::JoinUnion {
                left,
                right,
                left_key,
                right_key,
                ..
            } if *left == source => Some(*left_key),
            Plan::JoinUnion {
                left: _,
                right,
                right_key,
                ..
            } if *right == source => Some(*right_key),
            _ => None,
        }
    }

    fn column<'a>(row: &'a [i64], at: usize, source: u8) -> Result<&'a i64, EngineError> {
        row.get(at)
            .ok_or_else(|| Self::error("validate", Some(source), format!("column {at} absent")))
    }

    fn pending(&self, changes: &[Change]) -> Result<Pending, EngineError> {
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| Self::error("apply", None, "program not installed"))?;
        let mut pending = Pending::new();
        for change in changes {
            if change.weight != 1 && change.weight != -1 {
                return Err(Self::error(
                    "validate",
                    Some(change.source),
                    "weight must be +1 or -1",
                ));
            }
            let known = match plan {
                Plan::JoinUnion {
                    left,
                    right,
                    direct,
                    ..
                } => [*left, *right, *direct].contains(&change.source),
                Plan::GroupCountSum { source, .. } => *source == change.source,
            };
            if !known {
                return Err(Self::error(
                    "validate",
                    Some(change.source),
                    "source absent from plan",
                ));
            }
            let key = (change.source, change.row.id);
            let entry = pending.entry(key).or_insert_with(|| {
                let old = self
                    .rows
                    .get(&change.source)
                    .and_then(|rows| rows.get(&change.row.id))
                    .cloned();
                (old.clone(), old)
            });
            if change.weight == 1 {
                if entry.1.is_some() {
                    return Err(Self::error(
                        "validate",
                        Some(change.source),
                        "duplicate row ID",
                    ));
                }
                entry.1 = Some(change.row.cells.clone());
            } else {
                if entry.1.as_deref() != Some(change.row.cells.as_slice()) {
                    return Err(Self::error(
                        "validate",
                        Some(change.source),
                        "retracted row differs from stored row",
                    ));
                }
                entry.1 = None;
            }
        }
        for ((source, _), (_, after)) in &pending {
            if let Some(row) = after {
                match plan {
                    Plan::JoinUnion {
                        left,
                        right,
                        direct,
                        left_key,
                        right_key,
                        left_output,
                        right_output,
                        direct_output,
                    } => {
                        let needed: &[usize] = if source == left {
                            &[*left_key, *left_output]
                        } else if source == right {
                            &[*right_key, *right_output]
                        } else if source == direct {
                            direct_output
                        } else {
                            &[]
                        };
                        for &at in needed {
                            Self::column(row, at, *source)?;
                        }
                    }
                    Plan::GroupCountSum { group, value, .. } => {
                        Self::column(row, *group, *source)?;
                        Self::column(row, *value, *source)?;
                    }
                }
            }
        }
        Ok(pending)
    }

    fn diffs(pending: &Pending, source: u8) -> Vec<(Vec<i64>, i64)> {
        let mut out = Vec::new();
        for ((name, _), (before, after)) in pending {
            if *name != source || before == after {
                continue;
            }
            if let Some(row) = before {
                out.push((row.clone(), -1));
            }
            if let Some(row) = after {
                out.push((row.clone(), 1));
            }
        }
        out
    }

    fn old_at_key(&self, source: u8, key: i64) -> Vec<&[i64]> {
        self.indexes
            .get(&source)
            .and_then(|idx| idx.get(&key))
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.rows.get(&source)?.get(id).map(Vec::as_slice))
            .collect()
    }

    fn new_at_key<'a>(
        &'a self,
        source: u8,
        key: i64,
        key_col: usize,
        pending: &'a Pending,
    ) -> Vec<&'a [i64]> {
        let mut out = Vec::new();
        if let Some(ids) = self.indexes.get(&source).and_then(|idx| idx.get(&key)) {
            for id in ids {
                if pending.contains_key(&(source, *id)) {
                    continue;
                }
                if let Some(row) = self.rows.get(&source).and_then(|rows| rows.get(id)) {
                    out.push(row.as_slice());
                }
            }
        }
        for ((name, _), (_, after)) in pending {
            if *name == source {
                if let Some(row) = after {
                    if row[key_col] == key {
                        out.push(row.as_slice());
                    }
                }
            }
        }
        out
    }

    fn join_union_delta(
        &self,
        plan: &Plan,
        pending: &Pending,
    ) -> Result<BTreeMap<Vec<i64>, i64>, EngineError> {
        let Plan::JoinUnion {
            left,
            right,
            direct,
            left_key,
            right_key,
            left_output,
            right_output,
            direct_output,
        } = plan
        else {
            unreachable!()
        };
        let mut delta = BTreeMap::<Vec<i64>, i64>::new();
        for (row, sign) in Self::diffs(pending, *direct) {
            *delta
                .entry(vec![row[direct_output[0]], row[direct_output[1]]])
                .or_default() += sign;
        }
        // ΔL ⋈ R(old), then L(new) ⋈ ΔR. The cross-term enters exactly once.
        for (row, sign) in Self::diffs(pending, *left) {
            for other in self.old_at_key(*right, row[*left_key]) {
                *delta
                    .entry(vec![row[*left_output], other[*right_output]])
                    .or_default() += sign;
            }
        }
        for (row, sign) in Self::diffs(pending, *right) {
            for other in self.new_at_key(*left, row[*right_key], *left_key, pending) {
                *delta
                    .entry(vec![other[*left_output], row[*right_output]])
                    .or_default() += sign;
            }
        }
        delta.retain(|_, weight| *weight != 0);
        for (row, weight) in &delta {
            let after = self
                .support
                .get(row)
                .copied()
                .unwrap_or(0)
                .checked_add(*weight)
                .ok_or_else(|| Self::error("support", None, "weight overflow"))?;
            if after < 0 {
                return Err(Self::error("support", None, "negative support"));
            }
        }
        Ok(delta)
    }

    fn group_delta(
        &self,
        plan: &Plan,
        pending: &Pending,
    ) -> Result<BTreeMap<i64, (i64, i64)>, EngineError> {
        let Plan::GroupCountSum {
            source,
            group,
            value,
        } = plan
        else {
            unreachable!()
        };
        let mut delta = BTreeMap::<i64, (i64, i64)>::new();
        for (row, sign) in Self::diffs(pending, *source) {
            let entry = delta.entry(row[*group]).or_default();
            entry.0 = entry
                .0
                .checked_add(sign)
                .ok_or_else(|| Self::error("group", Some(*source), "count overflow"))?;
            entry.1 = entry
                .1
                .checked_add(
                    sign.checked_mul(row[*value])
                        .ok_or_else(|| Self::error("group", Some(*source), "sum overflow"))?,
                )
                .ok_or_else(|| Self::error("group", Some(*source), "sum overflow"))?;
        }
        for (key, (count, sum)) in &delta {
            let old = self.groups.get(key).copied().unwrap_or_default();
            let after_count = old
                .0
                .checked_add(*count)
                .ok_or_else(|| Self::error("group", Some(*source), "count overflow"))?;
            old.1
                .checked_add(*sum)
                .ok_or_else(|| Self::error("group", Some(*source), "sum overflow"))?;
            if after_count < 0 {
                return Err(Self::error("group", Some(*source), "negative count"));
            }
        }
        Ok(delta)
    }

    fn write_sources(&mut self, plan: &Plan, pending: Pending) {
        for ((source, id), (before, after)) in pending {
            if before == after {
                continue;
            }
            if let Some(key_col) = Self::key_column(plan, source) {
                if let Some(old) = &before {
                    let key = old[key_col];
                    if let Some(ids) = self
                        .indexes
                        .get_mut(&source)
                        .and_then(|idx| idx.get_mut(&key))
                    {
                        ids.remove(&id);
                    }
                }
                if let Some(new) = &after {
                    self.indexes
                        .entry(source)
                        .or_default()
                        .entry(new[key_col])
                        .or_default()
                        .insert(id);
                }
            }
            match after {
                Some(row) => {
                    self.rows.entry(source).or_default().insert(id, row);
                }
                None => {
                    self.rows.entry(source).or_default().remove(&id);
                }
            }
        }
    }
}

impl FrontierEngine for RustEngine {
    type Plan = Plan;
    type Change = Change;
    type Output = Vec<i64>;
    type Error = EngineError;

    fn install(&mut self, plan: Plan) -> Result<(), EngineError> {
        if self.plan.is_some() {
            return Err(Self::error("install", None, "program already installed"));
        }
        if let Plan::JoinUnion {
            left,
            right,
            direct,
            ..
        } = &plan
        {
            if left == right || left == direct || right == direct {
                return Err(Self::error("install", None, "source IDs must be distinct"));
            }
        }
        self.plan = Some(plan);
        Ok(())
    }

    fn apply(&mut self, changes: &[Change]) -> Result<Frontier, EngineError> {
        let plan = self
            .plan
            .clone()
            .ok_or_else(|| Self::error("apply", None, "program not installed"))?;
        let _span = tracing::info_span!(
            "rust_frontier",
            frontier = self.next_frontier,
            inputs = changes.len()
        )
        .entered();
        let pending = self.pending(changes)?;
        let output = match &plan {
            Plan::JoinUnion { .. } => {
                let delta = self.join_union_delta(&plan, &pending)?;
                let mut out = Vec::new();
                for (row, weight) in delta {
                    let old = self.support.get(&row).copied().unwrap_or(0);
                    let new = old + weight;
                    if old == 0 && new > 0 {
                        out.push(WeightedRow {
                            cells: row.clone(),
                            weight: 1,
                        });
                    }
                    if old > 0 && new == 0 {
                        out.push(WeightedRow {
                            cells: row.clone(),
                            weight: -1,
                        });
                    }
                    if new == 0 {
                        self.support.remove(&row);
                    } else {
                        self.support.insert(row, new);
                    }
                }
                out
            }
            Plan::GroupCountSum { .. } => {
                let delta = self.group_delta(&plan, &pending)?;
                let mut out = Vec::new();
                for (key, (count, sum)) in delta {
                    let old = self.groups.get(&key).copied().unwrap_or_default();
                    let new = (old.0 + count, old.1 + sum);
                    if old == new {
                        continue;
                    }
                    if old.0 > 0 {
                        out.push(WeightedRow {
                            cells: vec![key, old.0, old.1],
                            weight: -1,
                        });
                    }
                    if new.0 > 0 {
                        out.push(WeightedRow {
                            cells: vec![key, new.0, new.1],
                            weight: 1,
                        });
                    }
                    if new.0 == 0 {
                        self.groups.remove(&key);
                    } else {
                        self.groups.insert(key, new);
                    }
                }
                out
            }
        };
        self.write_sources(&plan, pending);
        let frontier = Frontier {
            id: self.next_frontier,
            changes: output,
        };
        self.next_frontier += 1;
        let process_peak_rss_bytes = hafley_observe::process_sample()
            .peak_rss_bytes
            .unwrap_or_default();
        tracing::info!(
            outputs = frontier.changes.len(),
            process_peak_rss_bytes,
            "rust_frontier_settled"
        );
        Ok(frontier)
    }

    fn snapshot(&self) -> Result<Vec<Vec<i64>>, EngineError> {
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| Self::error("read", None, "program not installed"))?;
        Ok(match plan {
            Plan::JoinUnion { .. } => self
                .support
                .iter()
                .filter(|(_, weight)| **weight > 0)
                .map(|(row, _)| row.clone())
                .collect(),
            Plan::GroupCountSum { .. } => self
                .groups
                .iter()
                .map(|(group, (count, sum))| vec![*group, *count, *sum])
                .collect(),
        })
    }

    fn teardown(&mut self) -> Result<(), EngineError> {
        *self = Self::default();
        Ok(())
    }
}
