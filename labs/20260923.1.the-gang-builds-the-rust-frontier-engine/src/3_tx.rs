//! Transaction scopes over the frontier engine.
//!
//! Every state mutation inside a frontier journals its inverse ([`Undo`]).
//! Scopes are journal marks: a savepoint rolls back by replaying the journal
//! in reverse to its mark. `Transaction` holds the engine exclusively for its
//! lifetime — the borrow checker, not a flag, excludes concurrent scopes.

use crate::storage::{Engine, Node};
use crate::types::{Cell, EngineError, EngineStats, FrontierEngine, ProgramId, RelId, Row, Stage};
use crate::types::{OutputDelta, RowId};

/// One inverted state mutation.
pub(crate) enum Undo {
    /// Undo: free the slot this insert allocated.
    Insert {
        /// Relation the row landed in.
        relation: RelId,
        /// Arena slot of the row.
        slot: u32,
        /// True when the slot came from the free list (vs arena growth).
        recycled: bool,
    },
    /// Undo: put the removed row back at its slot.
    Remove {
        /// Relation the row came from.
        relation: RelId,
        /// Arena slot to restore.
        slot: u32,
        /// Typed cells to restore.
        row: Row,
    },
    /// Undo: restore a node multiset entry.
    Counts {
        /// Program owning the node.
        program: ProgramId,
        /// Node arena index.
        node: u32,
        /// Row whose multiplicity changed.
        row: Row,
        /// Multiplicity before the touch; `None` when the row was absent.
        previous: Option<i64>,
    },
    /// Undo: restore a join key index entry.
    KeyIndex {
        /// Program owning the node.
        program: ProgramId,
        /// Node arena index.
        node: u32,
        /// Which side of the join.
        left: bool,
        /// Key cells.
        key: Row,
        /// Full child row whose multiplicity changed.
        row: Row,
        /// Multiplicity before the touch; `None` when absent.
        previous: Option<i64>,
    },
    /// Undo: restore aggregate group state.
    Group {
        /// Program owning the node.
        program: ProgramId,
        /// Node arena index.
        node: u32,
        /// Group key cells.
        key: Row,
        /// State before the touch; `None` when the group was absent.
        previous: Option<crate::storage::Group>,
    },
}

/// Replay the journal in reverse to `mark`, restoring exact state.
pub(crate) fn unwind(engine: &mut Engine, mark: usize) {
    while engine.journal.len() > mark {
        let undo = engine.journal.pop().expect("journal length checked");
        match undo {
            Undo::Insert {
                relation,
                slot,
                recycled,
            } => {
                let relation = engine
                    .relation_mut(relation, Stage::Maintain)
                    .expect("undo relation");
                let row = relation.rows[slot as usize]
                    .take()
                    .expect("undo insert row");
                if recycled {
                    relation.free.push(slot);
                } else {
                    debug_assert_eq!(slot as usize, relation.rows.len() - 1);
                    relation.rows.pop();
                }
                match relation.by_value.entry(row) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let ids = entry.get_mut();
                        let popped = ids.pop().expect("undo by_value entry");
                        debug_assert_eq!(popped, slot);
                        if ids.is_empty() {
                            entry.remove();
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(_) => {
                        unreachable!("undo of an indexed insert")
                    }
                }
            }
            Undo::Remove {
                relation,
                slot,
                row,
            } => {
                let relation = engine
                    .relation_mut(relation, Stage::Maintain)
                    .expect("undo relation");
                let popped = relation.free.pop().expect("undo free slot");
                debug_assert_eq!(popped, slot);
                relation.rows[slot as usize] = Some(row.clone());
                relation.by_value.entry(row).or_default().push(slot);
            }
            Undo::Counts {
                program,
                node,
                row,
                previous,
            } => {
                let state = program_state_mut(engine, program);
                let map = state.nodes[node as usize]
                    .counts_mut()
                    .expect("undo counts");
                match previous {
                    Some(value) => {
                        map.insert(row, value);
                    }
                    None => {
                        map.remove(&row);
                    }
                }
            }
            Undo::KeyIndex {
                program,
                node,
                left,
                key,
                row,
                previous,
            } => {
                let state = program_state_mut(engine, program);
                let index = match (&mut state.nodes[node as usize], left) {
                    (Node::Join { left_index, .. }, true) => &mut left_index.rows,
                    (Node::Join { right_index, .. }, false) => &mut right_index.rows,
                    _ => unreachable!("undo of a join index"),
                };
                match previous {
                    Some(value) => {
                        index.entry(key).or_default().insert(row, value);
                    }
                    None => {
                        if let Some(entry) = index.get_mut(&key) {
                            entry.remove(&row);
                            if entry.is_empty() {
                                index.remove(&key);
                            }
                        }
                    }
                }
            }
            Undo::Group {
                program,
                node,
                key,
                previous,
            } => {
                let state = program_state_mut(engine, program);
                let Node::Aggregate { groups, .. } = &mut state.nodes[node as usize] else {
                    unreachable!("undo of a group map");
                };
                match previous {
                    Some(group) => {
                        groups.insert(key, group);
                    }
                    None => {
                        groups.remove(&key);
                    }
                }
            }
        }
    }
    engine.journal.truncate(mark);
}

fn program_state_mut(engine: &mut Engine, program: ProgramId) -> &mut crate::storage::ProgramState {
    engine.programs[program as usize]
        .as_mut()
        .expect("undo of an installed program")
}

/// One journal mark plus the exclusive engine borrow. Settled by commit,
/// rollback, or a rollback on drop.
struct Scope<'e> {
    engine: &'e mut Engine,
    settled: bool,
}

impl Scope<'_> {
    fn open(engine: &mut Engine) -> Scope<'_> {
        engine.marks.push(engine.journal.len());
        Scope {
            engine,
            settled: false,
        }
    }

    fn pop_mark(&mut self) -> usize {
        self.engine.marks.pop().expect("scope mark")
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let mark = self.pop_mark();
            unwind(self.engine, mark);
        }
    }
}

/// One atomic scope over the engine: frontiers applied inside settle together,
/// or not at all. Dropping without `commit`/`rollback` rolls back.
pub struct Transaction<'e>(Scope<'e>);

impl Engine {
    /// Open a transaction. The engine stays borrowed until the transaction
    /// settles; an installed program's committed state is visible through the
    /// transaction the whole time.
    pub fn begin(&mut self) -> Transaction<'_> {
        Transaction(Scope::open(self))
    }
}

impl<'e> Transaction<'e> {
    /// Open a savepoint inside this transaction. Rolling back to it discards
    /// exactly the work applied after it; releasing keeps that work. Dropping
    /// without settling rolls the savepoint back.
    pub fn savepoint(&mut self) -> Savepoint<'_> {
        Savepoint(Scope::open(&mut *self.0.engine))
    }

    /// Apply one frontier inside the transaction. A frontier that errors is
    /// unwound by itself; the transaction stays open and the committed state
    /// stays readable.
    pub fn apply(
        &mut self,
        program: ProgramId,
        frontier: crate::types::Frontier,
    ) -> Result<Vec<OutputDelta>, EngineError> {
        apply_scoped(&mut *self.0.engine, program, frontier)
    }

    /// Visible rows with support counts through the open transaction.
    pub fn snapshot_support(
        &self,
        program: ProgramId,
        output: &str,
    ) -> Result<Vec<(Row, i64)>, EngineError> {
        self.0.engine.snapshot_support(program, output)
    }

    /// Read the committed snapshot through the open transaction.
    pub fn snapshot(&self, program: ProgramId, output: &str) -> Result<Vec<Row>, EngineError> {
        self.0.engine.snapshot(program, output)
    }

    /// Guardrail counts through the open transaction.
    pub fn stats(&self) -> EngineStats {
        self.0.engine.stats()
    }

    /// Read one source row through the open transaction.
    pub fn source_row(
        &self,
        relation: RelId,
        row_id: RowId,
    ) -> Result<Option<&[Cell]>, EngineError> {
        self.0.engine.source_row(relation, row_id)
    }

    /// Make every frontier applied in this scope durable.
    pub fn commit(mut self) -> Result<(), EngineError> {
        self.0.pop_mark();
        self.0.settled = true;
        Ok(())
    }

    /// Discard every frontier applied in this scope.
    pub fn rollback(mut self) -> Result<(), EngineError> {
        let mark = self.0.pop_mark();
        unwind(self.0.engine, mark);
        self.0.settled = true;
        Ok(())
    }
}

/// A nested scope inside a [`Transaction`].
pub struct Savepoint<'a>(Scope<'a>);

impl Savepoint<'_> {
    /// Apply one frontier inside the savepoint; visible between SAVEPOINT and
    /// ROLLBACK TO, discarded by it.
    pub fn apply(
        &mut self,
        program: ProgramId,
        frontier: crate::types::Frontier,
    ) -> Result<Vec<OutputDelta>, EngineError> {
        apply_scoped(&mut *self.0.engine, program, frontier)
    }

    /// Visible rows with support counts inside the open savepoint.
    pub fn snapshot_support(
        &self,
        program: ProgramId,
        output: &str,
    ) -> Result<Vec<(Row, i64)>, EngineError> {
        self.0.engine.snapshot_support(program, output)
    }

    /// Discard the work applied since the savepoint. The savepoint consumes
    /// itself, matching RELEASE after ROLLBACK TO.
    pub fn rollback_to(mut self) -> Result<(), EngineError> {
        let mark = self.0.pop_mark();
        unwind(self.0.engine, mark);
        self.0.settled = true;
        Ok(())
    }

    /// Keep the work applied since the savepoint and close it.
    pub fn release(mut self) {
        self.0.pop_mark();
        self.0.settled = true;
    }
}

/// One frontier inside any scope: its own mark, unwound alone on error.
fn apply_scoped(
    engine: &mut Engine,
    program: ProgramId,
    frontier: crate::types::Frontier,
) -> Result<Vec<OutputDelta>, EngineError> {
    engine.marks.push(engine.journal.len());
    let result = crate::apply::apply_frontier(engine, program, frontier);
    let mark = engine.marks.pop().expect("frontier mark");
    if result.is_err() {
        unwind(engine, mark);
    }
    result
}
