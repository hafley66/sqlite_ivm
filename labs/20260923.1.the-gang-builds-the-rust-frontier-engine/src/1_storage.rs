//! Engine state: source relations, installed programs, plan-node arena.
//!
//! Storage layout (reported contract):
//! - Relation: row arena `Vec<Option<Row>>` indexed by `RowId` (integer row
//!   identity, recycled LIFO), value index `HashMap<Row, Vec<RowId>>` keyed by
//!   typed cells (never serialized bytes), so duplicate values keep distinct
//!   row ids and deletion by value removes exactly one instance.
//! - Program: plan-node arena. Every maintained node owns its multiset
//!   (`counts`), joins own two key indexes, aggregates own a group map. These
//!   maps are the indexed arrangements; a map's identity is the owning node's
//!   arena index.

use std::collections::{HashMap, HashSet};

use crate::types::{
    Cell, EngineError, EngineStats, ErrorKind, FrontierEngine, OutputChange, OutputDelta, Program,
    ProgramId, RelId, Row, RowId, Sign, Stage,
};

/// Arena index of one plan node; doubles as the identity of the node's
/// maintained arrangements.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct NodeId(pub(crate) u32);

/// A source relation: row arena plus typed value index.
pub(crate) struct Relation {
    pub(crate) name: String,
    pub(crate) arity: usize,
    /// Arena indexed by `RowId.0`; `None` is a free slot.
    pub(crate) rows: Vec<Option<Row>>,
    /// Recycled row ids, LIFO.
    pub(crate) free: Vec<u32>,
    /// Value multiplicity: typed cells to the row ids holding that value.
    pub(crate) by_value: HashMap<Row, Vec<u32>>,
}

impl Relation {
    fn new(name: String, arity: usize) -> Self {
        Relation {
            name,
            arity,
            rows: Vec::new(),
            free: Vec::new(),
            by_value: HashMap::new(),
        }
    }

    pub(crate) fn live_rows(&self) -> usize {
        self.rows.iter().filter(|slot| slot.is_some()).count()
    }
}

/// One maintained key index for one side of a join: key cells to the
/// multiplicities of the full child rows under that key. Keyed by typed
/// cells only.
#[derive(Default, Debug)]
pub(crate) struct KeyIndex {
    /// Join key cells to full-row multiplicities under that key.
    pub(crate) rows: HashMap<Row, HashMap<Row, i64>>,
}

/// Aggregate group state: live count and running sum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Group {
    pub(crate) count: i64,
    pub(crate) sum: i64,
}

/// One plan node. Ids are the program's arena indexes.
pub(crate) enum Node {
    Scan {
        relation: RelId,
    },
    Union {
        inputs: Vec<NodeId>,
        counts: HashMap<Row, i64>,
    },
    Join {
        left: NodeId,
        right: NodeId,
        on_left: usize,
        on_right: usize,
        counts: HashMap<Row, i64>,
        left_index: KeyIndex,
        right_index: KeyIndex,
    },
    Aggregate {
        input: NodeId,
        group_by: Vec<usize>,
        count: bool,
        sum: Option<usize>,
        groups: HashMap<Row, Group>,
    },
    Project {
        input: NodeId,
        columns: Vec<usize>,
        counts: HashMap<Row, i64>,
    },
}

impl Node {
    /// The node's multiset, when it maintains one.
    pub(crate) fn counts(&self) -> Option<&HashMap<Row, i64>> {
        match self {
            Node::Union { counts, .. }
            | Node::Join { counts, .. }
            | Node::Project { counts, .. } => Some(counts),
            Node::Scan { .. } | Node::Aggregate { .. } => None,
        }
    }

    /// The node's multiset, mutable.
    pub(crate) fn counts_mut(&mut self) -> Option<&mut HashMap<Row, i64>> {
        match self {
            Node::Union { counts, .. }
            | Node::Join { counts, .. }
            | Node::Project { counts, .. } => Some(counts),
            Node::Scan { .. } | Node::Aggregate { .. } => None,
        }
    }

    /// Arrangements this node keeps between frontiers.
    pub(crate) fn arrangement_count(&self) -> usize {
        match self {
            Node::Scan { .. } => 0,
            Node::Union { .. } => 1,
            Node::Join { .. } => 3, // counts + two key indexes
            Node::Aggregate { .. } => 1,
            Node::Project { .. } => 1,
        }
    }
}

/// One output of an installed program.
pub(crate) struct OutputState {
    pub(crate) name: String,
    pub(crate) root: NodeId,
    /// True for distinct set outputs (support-counted transitions); false for
    /// aggregates, whose transitions are group-row diffs.
    pub(crate) distinct: bool,
}

pub(crate) struct ProgramState {
    pub(crate) nodes: Vec<Node>,
    pub(crate) outputs: Vec<OutputState>,
    pub(crate) dependencies: HashSet<RelId>,
    pub(crate) operators: usize,
    pub(crate) arrangements: usize,
}

/// The in-process frontier engine.
pub struct Engine {
    pub(crate) relations: Vec<Relation>,
    pub(crate) programs: Vec<Option<ProgramState>>,
    pub(crate) next_program: u32,
    /// Undo journal of the innermost open scope; empty at top level.
    pub(crate) journal: Vec<crate::tx::Undo>,
    /// Scope stack: journal lengths the scopes roll back to.
    pub(crate) marks: Vec<usize>,
    pub(crate) frontiers_applied: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// A fresh engine with no relations and no programs.
    pub fn new() -> Self {
        Engine {
            relations: Vec::new(),
            programs: Vec::new(),
            next_program: 0,
            journal: Vec::new(),
            marks: Vec::new(),
            frontiers_applied: 0,
        }
    }

    /// Define a source relation with fixed arity. Relation identity is the
    /// returned typed id; names exist for errors and observations only.
    pub fn define_relation(&mut self, name: impl Into<String>, arity: usize) -> RelId {
        let id = RelId(self.relations.len() as u32);
        self.relations.push(Relation::new(name.into(), arity));
        id
    }

    pub(crate) fn relation(&self, id: RelId, stage: Stage) -> Result<&Relation, EngineError> {
        self.relations
            .get(id.0 as usize)
            .ok_or_else(|| EngineError {
                stage,
                kind: ErrorKind::UnknownRelation(id),
                relation: None,
                output: None,
            })
    }

    pub(crate) fn relation_mut(
        &mut self,
        id: RelId,
        stage: Stage,
    ) -> Result<&mut Relation, EngineError> {
        self.relations
            .get_mut(id.0 as usize)
            .ok_or_else(|| EngineError {
                stage,
                kind: ErrorKind::UnknownRelation(id),
                relation: None,
                output: None,
            })
    }

    pub(crate) fn program(&self, id: ProgramId) -> Result<&ProgramState, EngineError> {
        self.programs
            .get(id as usize)
            .and_then(Option::as_ref)
            .ok_or(EngineError {
                stage: Stage::Validate,
                kind: ErrorKind::UnknownProgram(id),
                relation: None,
                output: None,
            })
    }

    /// Arity of a subplan output, resolved recursively.
    fn node_arity(&self, plan: &crate::types::PlanNode) -> Result<usize, EngineError> {
        use crate::types::PlanNode;
        match plan {
            PlanNode::Scan { relation } => Ok(self.relation(*relation, Stage::Install)?.arity),
            PlanNode::Union { inputs } => {
                let mut arity = None;
                for input in inputs {
                    let child = self.node_arity(input)?;
                    match arity {
                        None => arity = Some(child),
                        Some(same) if same == child => {}
                        Some(same) => {
                            return Err(EngineError {
                                stage: Stage::Install,
                                kind: ErrorKind::ArityMismatch {
                                    expected: same,
                                    actual: child,
                                },
                                relation: None,
                                output: None,
                            })
                        }
                    }
                }
                Ok(arity.unwrap_or(0))
            }
            PlanNode::Join { left, right, .. } => {
                Ok(self.node_arity(left)? + self.node_arity(right)?)
            }
            PlanNode::Aggregate {
                group_by,
                count,
                sum,
                ..
            } => Ok(group_by.len() + usize::from(*count) + usize::from(sum.is_some())),
            PlanNode::Project { columns, .. } => Ok(columns.len()),
            PlanNode::Difference { .. } => Err(EngineError {
                stage: Stage::Install,
                kind: ErrorKind::Unsupported {
                    shape: "difference",
                },
                relation: None,
                output: None,
            }),
        }
    }

    /// Validate one plan node recursively and build its arena subtree.
    fn build_node(
        &self,
        plan: &crate::types::PlanNode,
        nodes: &mut Vec<Node>,
        dependencies: &mut HashSet<RelId>,
    ) -> Result<NodeId, EngineError> {
        use crate::types::PlanNode;
        let node = match plan {
            PlanNode::Scan { relation } => {
                let arity = self.relation(*relation, Stage::Install)?.arity;
                let _ = arity; // arity lives on the relation; scans pass it through
                dependencies.insert(*relation);
                Node::Scan {
                    relation: *relation,
                }
            }
            PlanNode::Union { inputs } => {
                let mut children = Vec::with_capacity(inputs.len());
                for input in inputs {
                    children.push(self.build_node(input, nodes, dependencies)?);
                }
                Node::Union {
                    inputs: children,
                    counts: HashMap::new(),
                }
            }
            PlanNode::Join {
                left,
                right,
                on_left,
                on_right,
            } => {
                let left_arity = self.node_arity(left)?;
                let right_arity = self.node_arity(right)?;
                if on_left >= &left_arity {
                    return Err(EngineError {
                        stage: Stage::Install,
                        kind: ErrorKind::ArityMismatch {
                            expected: left_arity,
                            actual: on_left + 1,
                        },
                        relation: None,
                        output: None,
                    });
                }
                if on_right >= &right_arity {
                    return Err(EngineError {
                        stage: Stage::Install,
                        kind: ErrorKind::ArityMismatch {
                            expected: right_arity,
                            actual: on_right + 1,
                        },
                        relation: None,
                        output: None,
                    });
                }
                let left = self.build_node(left, nodes, dependencies)?;
                let right = self.build_node(right, nodes, dependencies)?;
                Node::Join {
                    left,
                    right,
                    on_left: *on_left,
                    on_right: *on_right,
                    counts: HashMap::new(),
                    left_index: KeyIndex::default(),
                    right_index: KeyIndex::default(),
                }
            }
            PlanNode::Aggregate {
                input,
                group_by,
                count,
                sum,
            } => {
                if group_by.is_empty() || (!count && sum.is_none()) {
                    return Err(EngineError {
                        stage: Stage::Install,
                        kind: ErrorKind::Unsupported {
                            shape: if group_by.is_empty() {
                                "global aggregate (no group key)"
                            } else {
                                "aggregate without COUNT or SUM"
                            },
                        },
                        relation: None,
                        output: None,
                    });
                }
                let input_arity = self.node_arity(input)?;
                let worst = group_by
                    .iter()
                    .chain(sum.iter())
                    .max()
                    .copied()
                    .unwrap_or(0);
                if worst >= input_arity {
                    return Err(EngineError {
                        stage: Stage::Install,
                        kind: ErrorKind::ArityMismatch {
                            expected: input_arity,
                            actual: worst + 1,
                        },
                        relation: None,
                        output: None,
                    });
                }
                let input = self.build_node(input, nodes, dependencies)?;
                Node::Aggregate {
                    input,
                    group_by: group_by.clone(),
                    count: *count,
                    sum: *sum,
                    groups: HashMap::new(),
                }
            }
            PlanNode::Project { input, columns } => {
                let input_arity = self.node_arity(input)?;
                let worst = columns.iter().max().copied().unwrap_or(0);
                if worst >= input_arity {
                    return Err(EngineError {
                        stage: Stage::Install,
                        kind: ErrorKind::ArityMismatch {
                            expected: input_arity,
                            actual: worst + 1,
                        },
                        relation: None,
                        output: None,
                    });
                }
                let input = self.build_node(input, nodes, dependencies)?;
                Node::Project {
                    input,
                    columns: columns.clone(),
                    counts: HashMap::new(),
                }
            }
            PlanNode::Difference { .. } => {
                return Err(EngineError {
                    stage: Stage::Install,
                    kind: ErrorKind::Unsupported {
                        shape: "difference",
                    },
                    relation: None,
                    output: None,
                })
            }
        };
        nodes.push(node);
        Ok(NodeId(nodes.len() as u32 - 1))
    }

    /// Named outputs of an installed program, in declaration order.
    pub fn outputs(&self, program: ProgramId) -> Result<Vec<String>, EngineError> {
        Ok(self
            .program(program)?
            .outputs
            .iter()
            .map(|o| o.name.clone())
            .collect())
    }

    /// Read one source row by identity. Ids are stable between the write that
    /// allocates them and the delete that frees them.
    pub fn source_row(
        &self,
        relation: RelId,
        row_id: RowId,
    ) -> Result<Option<&[Cell]>, EngineError> {
        let relation = self.relation(relation, Stage::Snapshot)?;
        Ok(relation
            .rows
            .get(row_id.0 as usize)
            .and_then(Option::as_deref))
    }

    /// Live source row ids of one relation, ascending. Distinct ids for equal
    /// values are the observable separation of row identity from value
    /// equality.
    pub fn source_row_ids(&self, relation: RelId) -> Result<Vec<RowId>, EngineError> {
        let relation = self.relation(relation, Stage::Snapshot)?;
        Ok(relation
            .rows
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_some())
            .map(|(index, _)| RowId(index as u32))
            .collect())
    }

    /// Visible rows with their support counts, sorted by row cells: the
    /// counterpart of the oracle's `support` view. Verification and
    /// observations read this; callers consume [`FrontierEngine::snapshot`].
    pub fn snapshot_support(
        &self,
        program: ProgramId,
        output: &str,
    ) -> Result<Vec<(Row, i64)>, EngineError> {
        let state = self.program(program)?;
        let output = state
            .outputs
            .iter()
            .find(|o| o.name == output)
            .ok_or_else(|| EngineError {
                stage: Stage::Snapshot,
                kind: ErrorKind::UnknownOutput(output.to_owned()),
                relation: None,
                output: Some(output.to_owned()),
            })?;
        let mut rows: Vec<(Row, i64)> = match &state.nodes[output.root.0 as usize] {
            Node::Aggregate {
                groups, count, sum, ..
            } => groups
                .iter()
                .map(|(key, group)| {
                    let mut row = key.clone();
                    if *count {
                        row.push(group.count);
                    }
                    if sum.is_some() {
                        row.push(group.sum);
                    }
                    (row, group.count)
                })
                .collect(),
            Node::Scan { relation } => self
                .relation(*relation, Stage::Snapshot)?
                .by_value
                .iter()
                .filter(|(_, ids)| !ids.is_empty())
                .map(|(row, ids)| (row.clone(), ids.len() as i64))
                .collect(),
            node => node
                .counts()
                .expect("union/join maintain counts")
                .iter()
                .filter(|(_, weight)| **weight > 0)
                .map(|(row, weight)| (row.clone(), *weight))
                .collect(),
        };
        rows.sort();
        Ok(rows)
    }
}

impl FrontierEngine for Engine {
    type ProgramId = ProgramId;
    type Error = EngineError;

    fn install(&mut self, program: &Program) -> Result<Self::ProgramId, Self::Error> {
        let mut nodes = Vec::new();
        let mut dependencies = HashSet::new();
        let mut outputs = Vec::with_capacity(program.outputs.len());
        for output in &program.outputs {
            let root = self
                .build_node(&output.plan, &mut nodes, &mut dependencies)
                .map_err(|mut error| {
                    if error.output.is_none() {
                        error.output = Some(output.name.clone());
                    }
                    error
                })?;
            let distinct = !matches!(output.plan, crate::types::PlanNode::Aggregate { .. });
            outputs.push(OutputState {
                name: output.name.clone(),
                root,
                distinct,
            });
        }
        let operators = nodes.len();
        let arrangements = nodes.iter().map(Node::arrangement_count).sum();
        let id = self.next_program;
        self.next_program += 1;
        if self.programs.len() == id as usize {
            self.programs.push(None);
        }
        crate::observ::emit_install(operators, arrangements, outputs.len());
        self.programs[id as usize] = Some(ProgramState {
            nodes,
            outputs,
            dependencies,
            operators,
            arrangements,
        });
        Ok(id)
    }

    fn uninstall(&mut self, program: Self::ProgramId) -> Result<(), Self::Error> {
        self.program(program)?;
        self.programs[program as usize] = None;
        Ok(())
    }

    fn apply(
        &mut self,
        program: Self::ProgramId,
        frontier: crate::types::Frontier,
    ) -> Result<Vec<OutputDelta>, Self::Error> {
        // Autocommit scope: one mark; an error unwinds to it.
        self.marks.push(self.journal.len());
        let result = crate::apply::apply_frontier(self, program, frontier);
        if result.is_err() {
            let mark = self.marks.pop().expect("autocommit mark");
            crate::tx::unwind(self, mark);
        } else {
            self.marks.pop().expect("autocommit mark");
        }
        result
    }

    fn snapshot(&self, program: Self::ProgramId, output: &str) -> Result<Vec<Row>, Self::Error> {
        let state = self.program(program)?;
        let output = state
            .outputs
            .iter()
            .find(|o| o.name == output)
            .ok_or_else(|| EngineError {
                stage: Stage::Snapshot,
                kind: ErrorKind::UnknownOutput(output.to_owned()),
                relation: None,
                output: Some(output.to_owned()),
            })?;
        let mut rows: Vec<Row> = match &state.nodes[output.root.0 as usize] {
            Node::Aggregate {
                groups, count, sum, ..
            } => groups
                .iter()
                .map(|(key, group)| {
                    let mut row = key.clone();
                    if *count {
                        row.push(group.count);
                    }
                    if sum.is_some() {
                        row.push(group.sum);
                    }
                    row
                })
                .collect(),
            Node::Scan { relation } => self
                .relation(*relation, Stage::Snapshot)?
                .by_value
                .iter()
                .filter(|(_, ids)| !ids.is_empty())
                .map(|(row, _)| row.clone())
                .collect(),
            node => node
                .counts()
                .expect("union/join maintain counts")
                .iter()
                .filter(|(_, weight)| **weight > 0)
                .map(|(row, _)| row.clone())
                .collect(),
        };
        rows.sort();
        Ok(rows)
    }

    fn stats(&self) -> EngineStats {
        let mut stats = EngineStats {
            installed_programs: self.programs.iter().filter(|p| p.is_some()).count(),
            ..EngineStats::default()
        };
        for program in self.programs.iter().flatten() {
            stats.operators += program.operators;
            stats.arrangements += program.arrangements;
        }
        stats.source_rows = self.relations.iter().map(Relation::live_rows).sum();
        stats
    }
}

/// Distinct-output transitions: visibility flips between pre and post support.
/// `counts` is already post-state; `delta` carries the net change per row.
pub(crate) fn distinct_transitions(
    counts: &HashMap<Row, i64>,
    delta: &HashMap<Row, i64>,
) -> Vec<OutputChange> {
    let mut changes: Vec<OutputChange> = delta
        .iter()
        .filter_map(|(row, weight)| {
            let post = counts.get(row).copied().unwrap_or(0);
            let pre = post - weight;
            match (pre > 0, post > 0) {
                (false, true) => Some(OutputChange {
                    sign: Sign::Plus,
                    row: row.clone(),
                }),
                (true, false) => Some(OutputChange {
                    sign: Sign::Minus,
                    row: row.clone(),
                }),
                _ => None,
            }
        })
        .collect();
    changes.sort_by(|a, b| a.row.cmp(&b.row));
    changes
}
