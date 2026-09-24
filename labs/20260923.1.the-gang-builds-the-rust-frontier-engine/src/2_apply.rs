//! One frontier, end to end:
//! 1. validate every change (stage `Validate`, before any write),
//! 2. consolidate the signed changes per relation at this one frontier,
//! 3. write source rows (arena + value index), journaling undo entries,
//! 4. propagate net deltas bottom-up through the plan; joins use the batch
//!    equation with the cross-term counted once, aggregates integrate per
//!    touched group,
//! 5. derive output transitions and return them; an error unwinds the journal.

use std::collections::{BTreeMap, HashMap};

use crate::storage::{Engine, Group, Node, NodeId, ProgramState};
use crate::tx::Undo;
use crate::types::{EngineError, ErrorKind, OutputChange, OutputDelta, RelId, Row, Sign, Stage};
/// Consolidated net change per relation: value rows to signed multiplicity.
type Net = BTreeMap<u32, BTreeMap<Row, i64>>;

pub(crate) fn apply_frontier(
    engine: &mut Engine,
    program: ProgramId32,
    frontier: crate::types::Frontier,
) -> Result<Vec<OutputDelta>, EngineError> {
    let span = crate::observ::frontier_span(&frontier.id, frontier.changes.len());
    let _entered = span.enter();

    // 1. Validate references and arities before any write.
    for change in &frontier.changes {
        let relation = engine.relation(change.relation, Stage::Validate)?;
        if relation.arity != change.row.len() {
            return Err(EngineError {
                stage: Stage::Validate,
                kind: ErrorKind::ArityMismatch {
                    expected: relation.arity,
                    actual: change.row.len(),
                },
                relation: Some(relation.name.clone()),
                output: None,
            });
        }
    }

    // 2. Consolidate signed changes at this frontier; zeros disappear.
    let mut consolidated: Net = BTreeMap::new();
    for change in &frontier.changes {
        let weight = change.sign.weight();
        let net = consolidated.entry(change.relation.0).or_default();
        match net.entry(change.row.clone()) {
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let value = slot.get_mut();
                *value += weight;
                if *value == 0 {
                    slot.remove();
                }
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                if weight != 0 {
                    slot.insert(weight);
                }
            }
        }
    }

    // 3. Net removals must be covered by rows that exist before the frontier.
    for (relation, net) in &consolidated {
        let relation = engine.relation(RelId(*relation), Stage::Validate)?;
        for (row, weight) in net {
            if *weight < 0 {
                let present = relation.by_value.get(row).map(|ids| ids.len()).unwrap_or(0) as i64;
                if present + weight < 0 {
                    return Err(EngineError {
                        stage: Stage::Validate,
                        kind: ErrorKind::Internal(format!(
                            "removal of {row:?} exceeds current multiplicity {present}"
                        )),
                        relation: Some(relation.name.clone()),
                        output: None,
                    });
                }
            }
        }
    }

    let changed: Vec<u32> = consolidated.keys().copied().collect();
    let relevant = {
        let state = engine.program(program)?;
        state.dependencies.iter().any(|r| changed.contains(&r.0))
    };
    if !relevant {
        engine.frontiers_applied += 1;
        let state = engine.program(program)?;
        return Ok(state
            .outputs
            .iter()
            .map(|o| OutputDelta {
                output: o.name.clone(),
                changes: Vec::new(),
            })
            .collect());
    }

    // 4. Source writes, one undo entry per row op.
    for (relation, net) in &consolidated {
        write_source(engine, RelId(*relation), net)?;
    }

    // 5. Propagate bottom-up. Field-split borrows: relations shared, the
    // program's node arena and the journal mutated.
    let engine_alias = &mut *engine;
    let crate::storage::Engine {
        relations,
        programs,
        journal,
        ..
    } = engine_alias;
    let state = programs[program as usize]
        .as_mut()
        .expect("program checked above");
    let mut propagation = Propagation {
        deltas: &consolidated,
        memo: HashMap::new(),
        journal,
        program,
    };
    let output_roots: Vec<(String, NodeId, bool)> = state
        .outputs
        .iter()
        .map(|o| (o.name.clone(), o.root, o.distinct))
        .collect();
    for (_, root, _) in &output_roots {
        propagation.run(state, *root)?;
    }

    // 6. Output transitions from post-state plus root delta.
    let mut deltas = Vec::with_capacity(output_roots.len());
    let mut total_changes = 0usize;
    for (name, root, distinct) in output_roots {
        let delta = propagation
            .memo
            .get(&root.0)
            .expect("root propagated")
            .clone();
        let changes = if distinct {
            let counts: HashMap<Row, i64> = match &state.nodes[root.0 as usize] {
                Node::Scan { relation } => relations[relation.0 as usize]
                    .by_value
                    .iter()
                    .map(|(row, ids)| (row.clone(), ids.len() as i64))
                    .collect(),
                node => node.counts().expect("union/join maintain counts").clone(),
            };
            crate::storage::distinct_transitions(&counts, &delta)
        } else {
            delta
                .iter()
                .map(|(row, weight)| OutputChange {
                    sign: if *weight > 0 { Sign::Plus } else { Sign::Minus },
                    row: row.clone(),
                })
                .collect()
        };
        let mut changes = changes;
        changes.sort_by(|a, b| a.row.cmp(&b.row));
        total_changes += changes.len();
        deltas.push(OutputDelta {
            output: name,
            changes,
        });
    }
    span.record("output_changes", total_changes as u64);
    engine_alias.frontiers_applied += 1;
    Ok(deltas)
}

type ProgramId32 = u32;

/// Bottom-up delta propagation over one program's node arena.
struct Propagation<'a> {
    deltas: &'a Net,
    memo: HashMap<u32, HashMap<Row, i64>>,
    journal: &'a mut Vec<Undo>,
    program: ProgramId32,
}

impl Propagation<'_> {
    fn run(
        &mut self,
        state: &mut ProgramState,
        node: NodeId,
    ) -> Result<HashMap<Row, i64>, EngineError> {
        if let Some(delta) = self.memo.get(&node.0) {
            return Ok(delta.clone());
        }
        let delta = self.step(state, node)?;
        self.memo.insert(node.0, delta.clone());
        Ok(delta)
    }

    fn step(
        &mut self,
        state: &mut ProgramState,
        node: NodeId,
    ) -> Result<HashMap<Row, i64>, EngineError> {
        match &state.nodes[node.0 as usize] {
            Node::Scan { relation } => {
                let relation = *relation;
                Ok(self
                    .deltas
                    .get(&relation.0)
                    .map(|net| net.iter().map(|(row, w)| (row.clone(), *w)).collect())
                    .unwrap_or_default())
            }
            Node::Union { inputs, .. } => {
                let inputs = inputs.clone();
                let mut total: HashMap<Row, i64> = HashMap::new();
                for child in inputs {
                    let child_delta = self.run(state, child)?;
                    for (row, weight) in child_delta {
                        accumulate(&mut total, row, weight);
                    }
                }
                let span = crate::observ::maintain_span(node.0, "union", total.len());
                let _guard = span.enter();
                self.apply_counts(state, node, &total)?;
                Ok(total)
            }
            Node::Join {
                left,
                right,
                on_left,
                on_right,
                ..
            } => {
                let (left, right, on_left, on_right) = (*left, *right, *on_left, *on_right);
                let dl = self.run(state, left)?;
                let dr = self.run(state, right)?;
                // Index updates first: term 1 reads right post-state, term 2
                // corrects left post-state back to pre-state via `dl`.
                self.index_update(state, node, true, &dl, on_left)?;
                self.index_update(state, node, false, &dr, on_right)?;
                let mut out: HashMap<Row, i64> = HashMap::new();
                // Term 1: ΔL ⋈ R_post.
                for (lrow, weight) in &dl {
                    let key = join_key(lrow, on_left);
                    for (rrow, rw) in self.index_rows(state, node, false, &key) {
                        let joined = [lrow.as_slice(), rrow.as_slice()].concat();
                        accumulate(&mut out, joined, weight * rw);
                    }
                }
                // Term 2: L_pre ⋈ ΔR (cross-term lands exactly once: Δr is in
                // R_post for term 1, and L_pre excludes Δl for term 2).
                for (rrow, weight) in &dr {
                    let key = join_key(rrow, on_right);
                    for (lrow, lw) in self.index_rows(state, node, true, &key) {
                        let pre = lw - dl.get(&lrow).copied().unwrap_or(0);
                        if pre != 0 {
                            let joined = [lrow.as_slice(), rrow.as_slice()].concat();
                            accumulate(&mut out, joined, pre * weight);
                        }
                    }
                }
                let span = crate::observ::maintain_span(node.0, "join", out.len());
                let _guard = span.enter();
                self.apply_counts(state, node, &out)?;
                Ok(out)
            }
            Node::Aggregate {
                input,
                group_by,
                count,
                sum,
                ..
            } => {
                let (input, group_by, count, sum) = (input.clone(), group_by.clone(), *count, *sum);
                let incoming = self.run(state, input)?;
                let mut counts: BTreeMap<Row, i64> = BTreeMap::new();
                let mut sums: BTreeMap<Row, i64> = BTreeMap::new();
                for (row, weight) in &incoming {
                    let key: Row = group_by.iter().map(|col| row[*col]).collect();
                    accumulate_btree(&mut counts, key.clone(), *weight);
                    if let Some(col) = sum {
                        accumulate_btree(&mut sums, key, *weight * row[col]);
                    }
                }
                let mut out: HashMap<Row, i64> = HashMap::new();
                let keys: std::collections::BTreeSet<Row> =
                    counts.keys().chain(sums.keys()).cloned().collect();
                // Re-borrow the group map per key; `keys` is owned.
                for key in &keys {
                    let dc = counts.get(key).copied().unwrap_or(0);
                    let ds = sums.get(key).copied().unwrap_or(0);
                    let Node::Aggregate { groups, .. } = &mut state.nodes[node.0 as usize] else {
                        unreachable!("aggregate node")
                    };
                    let previous = groups.get(key).copied();
                    let old = previous.unwrap_or(Group { count: 0, sum: 0 });
                    let new = Group {
                        count: old.count + dc,
                        sum: old.sum + ds,
                    };
                    if new.count < 0 {
                        return Err(EngineError {
                            stage: Stage::Maintain,
                            kind: ErrorKind::Internal(format!("group {key:?} count went negative")),
                            relation: None,
                            output: None,
                        });
                    }
                    if Some(new) == previous {
                        continue;
                    }
                    self.journal.push(Undo::Group {
                        program: self.program,
                        node: node.0,
                        key: key.clone(),
                        previous,
                    });
                    if new.count == 0 {
                        groups.remove(key);
                    } else {
                        groups.insert(key.clone(), new);
                    }
                    let row_of = |group: Group| {
                        let mut row = key.clone();
                        if count {
                            row.push(group.count);
                        }
                        if sum.is_some() {
                            row.push(group.sum);
                        }
                        row
                    };
                    match (previous.map(|g| g.count > 0), new.count > 0) {
                        (Some(true), true) => {
                            accumulate(&mut out, row_of(old), -1);
                            accumulate(&mut out, row_of(new), 1);
                        }
                        (Some(true), false) => {
                            accumulate(&mut out, row_of(old), -1);
                        }
                        (Some(false) | None, true) => {
                            accumulate(&mut out, row_of(new), 1);
                        }
                        (Some(false) | None, false) => {}
                    }
                }
                let span = crate::observ::maintain_span(node.0, "aggregate", out.len());
                let _guard = span.enter();
                Ok(out)
            }
            Node::Project { input, columns, .. } => {
                let (input, columns) = (input.clone(), columns.clone());
                let incoming = self.run(state, input)?;
                let mut out: HashMap<Row, i64> = HashMap::new();
                for (row, weight) in &incoming {
                    let projected: Row = columns.iter().map(|col| row[*col]).collect();
                    accumulate(&mut out, projected, *weight);
                }
                {
                    let span = crate::observ::maintain_span(node.0, "project", out.len());
                    let _guard = span.enter();
                    self.apply_counts(state, node, &out)?;
                }
                Ok(out)
            }
        }
    }

    /// Fold one net delta into a node's multiset, journaling previous values.
    fn apply_counts(
        &mut self,
        state: &mut ProgramState,
        node: NodeId,
        delta: &HashMap<Row, i64>,
    ) -> Result<(), EngineError> {
        for (row, weight) in delta {
            let map = state.nodes[node.0 as usize]
                .counts_mut()
                .expect("apply_counts on a counted node");
            let previous = map.get(row).copied();
            let next = previous.unwrap_or(0) + weight;
            if next < 0 {
                return Err(EngineError {
                    stage: Stage::Maintain,
                    kind: ErrorKind::Internal(format!(
                        "support for {row:?} went negative ({next})"
                    )),
                    relation: None,
                    output: None,
                });
            }
            self.journal.push(Undo::Counts {
                program: self.program,
                node: node.0,
                row: row.clone(),
                previous,
            });
            if next == 0 {
                map.remove(row);
            } else {
                map.insert(row.clone(), next);
            }
        }
        Ok(())
    }

    /// Fold one child delta into one side of a join key index.
    fn index_update(
        &mut self,
        state: &mut ProgramState,
        node: NodeId,
        left: bool,
        delta: &HashMap<Row, i64>,
        key_col: usize,
    ) -> Result<(), EngineError> {
        for (row, weight) in delta {
            let key = join_key(row, key_col);
            let index = index_rows_mut(state, node, left);
            let entry = index.entry(key.clone()).or_default();
            let previous = entry.get(row).copied();
            let next = previous.unwrap_or(0) + weight;
            if next < 0 {
                return Err(EngineError {
                    stage: Stage::Maintain,
                    kind: ErrorKind::Internal(format!(
                        "join index under {key:?} went negative ({next})"
                    )),
                    relation: None,
                    output: None,
                });
            }
            self.journal.push(Undo::KeyIndex {
                program: self.program,
                node: node.0,
                left,
                key: key.clone(),
                row: row.clone(),
                previous,
            });
            if next == 0 {
                entry.remove(row);
            } else {
                entry.insert(row.clone(), next);
            }
            if entry.is_empty() {
                let index = index_rows_mut(state, node, left);
                index.remove(&key);
            }
        }
        Ok(())
    }

    /// Rows of one join side under one key, post-state, as (row, multiplicity).
    fn index_rows(
        &self,
        state: &ProgramState,
        node: NodeId,
        left: bool,
        key: &Row,
    ) -> Vec<(Row, i64)> {
        let node_ref = &state.nodes[node.0 as usize];
        let index = match (node_ref, left) {
            (Node::Join { left_index, .. }, true) => &left_index.rows,
            (Node::Join { right_index, .. }, false) => &right_index.rows,
            _ => unreachable!("index_rows on a join"),
        };
        index
            .get(key)
            .map(|entry| entry.iter().map(|(row, w)| (row.clone(), *w)).collect())
            .unwrap_or_default()
    }
}

/// One typed join key: the key column's cell as a single-cell row.
fn join_key(row: &[crate::types::Cell], col: usize) -> Row {
    vec![row[col]]
}

fn index_rows_mut(
    state: &mut ProgramState,
    node: NodeId,
    left: bool,
) -> &mut HashMap<Row, HashMap<Row, i64>> {
    let node_ref = &mut state.nodes[node.0 as usize];
    match (node_ref, left) {
        (Node::Join { left_index, .. }, true) => &mut left_index.rows,
        (Node::Join { right_index, .. }, false) => &mut right_index.rows,
        _ => unreachable!("index_rows_mut on a join"),
    }
}

fn accumulate(map: &mut HashMap<Row, i64>, row: Row, weight: i64) {
    match map.entry(row) {
        std::collections::hash_map::Entry::Occupied(mut slot) => {
            let value = slot.get_mut();
            *value += weight;
            if *value == 0 {
                slot.remove();
            }
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            if weight != 0 {
                slot.insert(weight);
            }
        }
    }
}

fn accumulate_btree(map: &mut BTreeMap<Row, i64>, row: Row, weight: i64) {
    match map.entry(row) {
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            let value = slot.get_mut();
            *value += weight;
            if *value == 0 {
                slot.remove();
            }
        }
        std::collections::btree_map::Entry::Vacant(slot) => {
            if weight != 0 {
                slot.insert(weight);
            }
        }
    }
}

/// Apply a consolidated relation delta to the source tables.
fn write_source(
    engine: &mut Engine,
    relation: RelId,
    net: &BTreeMap<Row, i64>,
) -> Result<(), EngineError> {
    for (row, weight) in net {
        for _ in 0..(*weight).max(0) {
            insert_row(engine, relation, row)?;
        }
        for _ in 0..(-*weight).max(0) {
            remove_row(engine, relation, row)?;
        }
    }
    Ok(())
}

fn insert_row(
    engine: &mut Engine,
    relation: RelId,
    row: &Row,
) -> Result<crate::types::RowId, EngineError> {
    let relation_state = engine.relation_mut(relation, Stage::Source)?;
    let (slot, recycled) = match relation_state.free.pop() {
        Some(slot) => (slot as usize, true),
        None => {
            relation_state.rows.push(None);
            (relation_state.rows.len() - 1, false)
        }
    };
    relation_state.rows[slot] = Some(row.clone());
    relation_state
        .by_value
        .entry(row.clone())
        .or_default()
        .push(slot as u32);
    engine.journal.push(Undo::Insert {
        relation,
        slot: slot as u32,
        recycled,
    });
    Ok(crate::types::RowId(slot as u32))
}

fn remove_row(
    engine: &mut Engine,
    relation: RelId,
    row: &Row,
) -> Result<Option<crate::types::RowId>, EngineError> {
    let relation_state = engine.relation_mut(relation, Stage::Source)?;
    let slot = match relation_state.by_value.entry(row.clone()) {
        std::collections::hash_map::Entry::Occupied(mut slot) => {
            let ids = slot.get_mut();
            let popped = ids.pop().expect("validated multiplicity");
            if ids.is_empty() {
                slot.remove();
            }
            popped
        }
        std::collections::hash_map::Entry::Vacant(_) => return Ok(None),
    };
    relation_state.rows[slot as usize] = None;
    relation_state.free.push(slot);
    engine.journal.push(Undo::Remove {
        relation,
        slot,
        row: row.clone(),
    });
    Ok(Some(crate::types::RowId(slot)))
}
