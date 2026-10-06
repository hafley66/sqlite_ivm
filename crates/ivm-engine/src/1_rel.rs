//! The operator algebra each engine implements, and the one lowering written against it.

use ivm_ir::*;
use smallvec::{smallvec, SmallVec};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Check,
    Install,
    Settle,
    Snapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Unsupported(&'static str),
    UnknownRel(RelId),
    UnknownNode(NodeId),
    Arity { expected: usize, actual: usize },
    PresentInsert(Row),
    Worker(String),
    /// A LetRec with `limit = n` still changed a variable in body evaluation `n + 1`; `rel` names
    /// the LetRec's first id.
    LetRecLimit(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub stage: Stage,
    pub rel: Option<RelId>,
    pub kind: ErrorKind,
}

impl EngineError {
    pub fn new(stage: Stage, rel: Option<RelId>, kind: ErrorKind) -> Self {
        Self { stage, rel, kind }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} rel {:?}: {:?}", self.stage, self.rel, self.kind)
    }
}

impl std::error::Error for EngineError {}

/// Work observed during the most recent successful settle. `None` means the
/// engine or installed planner does not measure that field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeltaRows {
    pub filter: Option<u64>,
    pub join: Option<u64>,
    pub antijoin: Option<u64>,
    pub reduce: Option<u64>,
    pub topk: Option<u64>,
    pub window: Option<u64>,
    pub mint: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Number of nonzero net output delta rows returned by settle.
    pub rows_written: u64,
    /// Rows emitted by each IR operator, before downstream operators run.
    pub delta_rows: DeltaRows,
    /// LetRec rounds that produced a nonempty next delta, summed across scopes.
    pub rounds: Option<u64>,
    /// New dictionary entries created during settle.
    pub interned: Option<u64>,
    /// SQLite statements executed during settle.
    pub statements: Option<u64>,
}

impl Counters {
    pub fn measured() -> Self {
        Self {
            delta_rows: DeltaRows {
                filter: Some(0), join: Some(0), antijoin: Some(0), reduce: Some(0),
                topk: Some(0), window: Some(0), mint: Some(0),
            },
            rounds: Some(0),
            interned: Some(0),
            statements: Some(0),
            ..Self::default()
        }
    }
}

/// Lifecycle every engine implements; the oracle harness is written against this trait only.
/// Each engine opens and owns its storage in `install`.
pub trait Engine: Sized {
    fn install(program: &Program) -> Result<Self, EngineError>;
    /// `install` for a caller that never reads per-operator work: `counters()` may report
    /// `None` for `delta_rows` and `rounds`. Engines whose counters cost nothing keep this default.
    fn install_unmeasured(program: &Program) -> Result<Self, EngineError> {
        Self::install(program)
    }
    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError>;
    fn counters(&self) -> Counters;
    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError>;
    fn intern_snapshot(&self, functor: RelId) -> Result<Vec<(Row, W)>, EngineError>;
    /// `intern_snapshot` of each functor, in order.
    fn intern_snapshots(&self, functors: &[RelId]) -> Result<Vec<Vec<(Row, W)>>, EngineError> {
        functors.iter().map(|functor| self.intern_snapshot(*functor)).collect()
    }
    fn intern_text(&mut self, text: &str) -> Result<Cell, EngineError>;
    /// Interns constructor terms `functor(args)` in order and returns their ids, as a Mint of
    /// the same row would. Each argument is an interned term, a text cell or a raw value.
    /// The constructor's rows hold the new terms from the next settle on.
    fn intern_terms(&mut self, terms: &[(RelId, Row)]) -> Result<Vec<Cell>, EngineError>;
    /// Constructors `(name, argument columns)` no operator reads: one id each (a known name keeps
    /// its own), accepted by `intern_terms` from then on.
    fn declare_constructors(&mut self, ctors: &[(String, Vec<Ty>)]) -> Result<Vec<RelId>, EngineError> {
        let _ = ctors;
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("declare_constructors")))
    }
    fn text(&self, id: Cell) -> Result<Option<String>, EngineError>;
    fn intern_any(&mut self, value: &ivm_ir::AnyValue) -> Result<Cell, EngineError>;
    fn any_value(&self, id: Cell) -> Result<ivm_ir::AnyValue, EngineError>;
    /// Whether `mark` and `rewind` are supported. A caller asks before marking and keeps
    /// retracting rows on an engine that answers `false`.
    fn rewinds(&self) -> bool {
        false
    }
    /// Records the current state of every relation, term and text; a later `rewind` returns to
    /// it. A second `mark` replaces the first.
    fn mark(&mut self) -> Result<(), EngineError> {
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("mark")))
    }
    /// Returns every relation, term and text to the last `mark`, discarding every settle since.
    /// Cells interned since the mark are freed and may be reissued. The mark stays.
    fn rewind(&mut self) -> Result<(), EngineError> {
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("rewind")))
    }
}

pub trait Rel {
    type C: Clone;
    fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError>;
    fn mint(&mut self, c: Self::C, functor: RelId, args: &[ColId]) -> Result<Self::C, EngineError>;
    fn str_cons(&mut self, c: Self::C, mode: &StrMode) -> Result<Self::C, EngineError>;
    fn str_op(&mut self, c: Self::C, op: StrOp, args: &[ColId]) -> Result<Self::C, EngineError>;
    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId], input_types: &[Ty]) -> Self::C;
    fn union(&mut self, cs: Vec<Self::C>) -> Self::C;
    fn negate(&mut self, c: Self::C) -> Self::C;
    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>]) -> Result<Self::C, EngineError>;
    /// True: `lower_node` builds an `Mfp` that only projects a two-input `Join` as one
    /// `join_project`, and never builds that join's full-width output for it.
    fn fuses_join_project(&self) -> bool {
        false
    }
    /// `join` whose output rows hold only the `project` columns of the concatenated row.
    fn join_project(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>], project: &[ColId]) -> Result<Self::C, EngineError> {
        let c = self.join(cs, eq, types)?;
        let all: Vec<Ty> = types.iter().flatten().copied().collect();
        Ok(self.mfp(c, &[], &[], project, &all))
    }
    /// `c ⋈ functor` on `c.col = functor.c0`: each row extended by the constructor row of the term
    /// its `col` names. A row names only terms minted before it, so the engine reads the
    /// constructor's current rows and no constructor change ever drives the result.
    fn decode(&mut self, c: Self::C, functor: RelId, col: ColId, types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
        let k = self.get(functor)?;
        self.join(vec![c, k], &[vec![(0, col), (1, 0)]], types)
    }
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C;
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg], input_types: &[Ty]) -> Self::C;
    fn threshold(&mut self, c: Self::C) -> Self::C;
    fn topk(&mut self, c: Self::C, key: &[ColId], order: &[Order], limit: u32, input_types: &[Ty]) -> Result<Self::C, EngineError>;
    fn window(&mut self, c: Self::C, partition: &[ColId], order: &[Order], func: &WinFn, input_types: &[Ty]) -> Result<Self::C, EngineError>;
    /// Engine-owned fixpoint: returns one collection per `rec.ids`, built with `lower_node` on the engine's inner algebra.
    /// `outer`: the outer-scope node memo the `Let` strata share; nodes the fixpoint lowers in the
    /// outer scope go through it, so a node every stratum reads is one collection.
    fn letrec(&mut self, p: &Program, rec: &LetRec, defined: &[(RelId, Self::C)], outer: &mut Vec<Option<Self::C>>) -> Result<Vec<Self::C>, EngineError>;
    fn output(&mut self, rel: RelId, c: Self::C);
    /// `p.node_types(id)`; an engine that lowers many nodes may memoize it.
    fn node_types(&mut self, p: &Program, id: NodeId) -> Option<Vec<Ty>> {
        p.node_types(id)
    }
    /// Called by `lower_node` on every node it builds, before memoizing; traced engines tap `c` here.
    fn observe(&mut self, _id: NodeId, c: Self::C) -> Self::C {
        c
    }
}

/// Scalar semantics shared by every engine: comparisons and logic yield 0 or 1.
pub fn eval(expr: &Expr, row: &[Cell]) -> Cell {
    eval_with(expr, row, &|_, _| panic!("TermLt requires a dictionary comparator"))
}

pub fn eval_with(expr: &Expr, row: &[Cell], term_lt: &dyn Fn(Cell, Cell) -> bool) -> Cell {
    eval_with_text(expr, row, term_lt, &|_| panic!("Text requires a dictionary"), &|_| panic!("Term requires a dictionary"), &|| panic!("StrNil requires a dictionary"))
}

enum ExprStep<'e> {
    Enter(&'e Expr),
    Apply(&'e Func, usize),
}

/// Post-order over `expr` with explicit stacks: `leaf` values a column, literal, text or
/// `StrNil`; `apply` values a call from its operands' values, left to right.
fn walk_expr<V: Copy>(expr: &Expr, leaf: impl Fn(&Expr) -> V, apply: impl Fn(&Func, &[V]) -> V) -> V {
    // Trees two calls deep, the common filter and map shapes, skip the stacks.
    let is_leaf = |e: &Expr| !matches!(e, Expr::Call(func, _) if *func != Func::StrNil);
    let is_flat = |e: &Expr| match e {
        Expr::Call(func, args) if *func != Func::StrNil => args.len() <= 2 && args.iter().all(is_leaf),
        _ => true,
    };
    let flat = |e: &Expr| match e {
        Expr::Call(func, args) if *func != Func::StrNil => {
            let values: SmallVec<[V; 2]> = args.iter().map(&leaf).collect();
            apply(func, &values)
        }
        e => leaf(e),
    };
    match expr {
        e if is_flat(e) => return flat(e),
        Expr::Call(func, args) if args.len() <= 2 && args.iter().all(is_flat) => {
            let values: SmallVec<[V; 2]> = args.iter().map(flat).collect();
            return apply(func, &values);
        }
        _ => {}
    }
    let mut steps: SmallVec<[ExprStep; 16]> = smallvec![ExprStep::Enter(expr)];
    let mut values: SmallVec<[V; 16]> = SmallVec::new();
    while let Some(step) = steps.pop() {
        match step {
            ExprStep::Enter(Expr::Call(func, args)) if *func != Func::StrNil => {
                steps.push(ExprStep::Apply(func, args.len()));
                steps.extend(args.iter().rev().map(ExprStep::Enter));
            }
            ExprStep::Enter(e) => values.push(leaf(e)),
            ExprStep::Apply(func, arity) => {
                let base = values.len() - arity;
                let value = apply(func, &values[base..]);
                values.truncate(base);
                values.push(value);
            }
        }
    }
    values[0]
}

/// `text(i)` and `term(i)` are the cells of `Program::texts[i]` and `Program::terms[i]`.
pub fn eval_with_text(expr: &Expr, row: &[Cell], term_lt: &dyn Fn(Cell, Cell) -> bool, text: &dyn Fn(u32) -> Cell, term: &dyn Fn(u32) -> Cell, nil: &dyn Fn() -> Cell) -> Cell {
    let leaf = |e: &Expr| match e {
        Expr::Col(c) => row[*c as usize],
        Expr::Lit(v) => *v,
        Expr::Text(index) => text(*index),
        Expr::Term(index) => term(*index),
        Expr::Call(..) => nil(),
    };
    walk_expr(expr, leaf, |func, a: &[Cell]| match func {
        Func::Eq => (a[0] == a[1]) as Cell,
        Func::Ne => (a[0] != a[1]) as Cell,
        Func::Lt => (a[0] < a[1]) as Cell,
        Func::Le => (a[0] <= a[1]) as Cell,
        Func::Gt => (a[0] > a[1]) as Cell,
        Func::Ge => (a[0] >= a[1]) as Cell,
        Func::Add => a[0].wrapping_add(a[1]),
        Func::Sub => a[0].wrapping_sub(a[1]),
        Func::And => (a[0] != 0 && a[1] != 0) as Cell,
        Func::Or => (a[0] != 0 || a[1] != 0) as Cell,
        Func::Not => (a[0] == 0) as Cell,
        Func::TermLt => term_lt(a[0], a[1]) as Cell,
        Func::StrNil => unreachable!(),
    })
}

/// Each value carries its `expr_type`, computed bottom-up in the same walk.
pub fn eval_typed_with_text(
    expr: &Expr, row: &[Cell], types: &[Ty],
    term_lt: &dyn Fn(Cell, Cell) -> bool,
    text: &dyn Fn(u32) -> Cell, term: &dyn Fn(u32) -> Cell, nil: &dyn Fn() -> Cell,
    compare: &dyn Fn(Ty, Cell, Ty, Cell) -> std::cmp::Ordering,
) -> Cell {
    use std::cmp::Ordering;
    let leaf = |e: &Expr| match e {
        Expr::Col(c) => (row[*c as usize], types.get(*c as usize).copied()),
        Expr::Lit(v) => (*v, Some(Ty::Int)),
        Expr::Text(index) => (text(*index), Some(Ty::Text)),
        Expr::Term(index) => (term(*index), Some(Ty::Id)),
        Expr::Call(..) => (nil(), Some(Ty::Text)),
    };
    let apply = |func: &Func, a: &[(Cell, Option<Ty>)]| {
        let cmp = || compare(a[0].1.unwrap(), a[0].0, a[1].1.unwrap(), a[1].0);
        let ty = match (func, a) {
            (Func::Add | Func::Sub, [(_, Some(left)), (_, Some(right)), ..]) => {
                Some(if *left == Ty::Real || *right == Ty::Real { Ty::Real } else { Ty::Int })
            }
            (Func::Add | Func::Sub, _) => None,
            _ => Some(Ty::Int),
        };
        let value = match func {
            Func::Eq => (cmp() == Ordering::Equal) as Cell,
            Func::Ne => (cmp() != Ordering::Equal) as Cell,
            Func::Lt => (cmp() == Ordering::Less) as Cell,
            Func::Le => (cmp() != Ordering::Greater) as Cell,
            Func::Gt => (cmp() == Ordering::Greater) as Cell,
            Func::Ge => (cmp() != Ordering::Less) as Cell,
            Func::Add | Func::Sub if ty == Some(Ty::Real) => {
                let number = |i: usize| if a[i].1 == Some(Ty::Real) {
                    f64::from_bits(a[i].0 as u64)
                } else { a[i].0 as f64 };
                (if *func == Func::Add { number(0) + number(1) } else { number(0) - number(1) }).to_bits() as i64
            }
            Func::Add => a[0].0.wrapping_add(a[1].0),
            Func::Sub => a[0].0.wrapping_sub(a[1].0),
            Func::And => (a[0].0 != 0 && a[1].0 != 0) as Cell,
            Func::Or => (a[0].0 != 0 || a[1].0 != 0) as Cell,
            Func::Not => (a[0].0 == 0) as Cell,
            Func::TermLt => term_lt(a[0].0, a[1].0) as Cell,
            Func::StrNil => unreachable!(),
        };
        (value, ty)
    };
    walk_expr(expr, leaf, apply).0
}

pub fn lower<A: Rel>(p: &Program, a: &mut A) -> Result<(), EngineError> {
    let mut nodes: Vec<Option<A::C>> = vec![None; p.nodes.len()];
    let mut defined: Vec<(RelId, A::C)> = Vec::new();
    for stratum in &p.strata {
        match stratum {
            Stratum::Let { id, body } => {
                let c = lower_node(p, a, &mut nodes, &defined, *body)?;
                defined.push((*id, c));
            }
            Stratum::LetRec(rec) => {
                let cs = a.letrec(p, rec, &defined, &mut nodes)?;
                defined.extend(rec.ids.iter().copied().zip(cs));
            }
        }
    }
    for out in &p.outputs {
        let c = defined
            .iter()
            .find(|(id, _)| id == out)
            .map(|(_, c)| c.clone())
            .ok_or_else(|| EngineError::new(Stage::Install, Some(*out), ErrorKind::UnknownRel(*out)))?;
        a.output(*out, c);
    }
    Ok(())
}

/// A two-input join whose one equivalence pairs a constructor's id column with a column of the
/// other input: `(constructor side, other input's column, constructor)`.
fn decoded<C>(p: &Program, defined: &[(RelId, C)], inputs: &[NodeId], equivalences: &[Vec<(u8, ColId)>]) -> Option<(usize, ColId, RelId)> {
    let [class] = equivalences else { return None };
    let [first, second] = class.as_slice() else { return None };
    if inputs.len() != 2 || first.0 == second.0 { return None; }
    [first, second].into_iter().find_map(|&(side, col)| {
        let Op::Get(rel) = p.nodes.get(*inputs.get(side as usize)? as usize)? else { return None };
        let constructor = p.rel(*rel)?.kind == RelKind::Constructor && !defined.iter().any(|(id, _)| id == rel);
        let other = if first.0 == side { second } else { first };
        (constructor && col == 0).then_some((side as usize, other.1, *rel))
    })
}

/// A two-input `Join` that is no constructor decode, projected: the Join's own `project`, or an
/// `Mfp { project }` with no filter or map over a Join without one. `(join inputs, equivalences,
/// project)`.
pub fn projected_join<'p, C>(p: &'p Program, defined: &[(RelId, C)], op: &'p Op) -> Option<(&'p [NodeId], &'p [Vec<(u8, ColId)>], &'p [ColId])> {
    let (inputs, equivalences, project) = match op {
        Op::Join { inputs, equivalences, project } if !project.is_empty() => (inputs, equivalences, project),
        Op::Mfp { input, filter, map, project } => {
            if !filter.is_empty() || !map.is_empty() || project.is_empty() { return None; }
            let Op::Join { inputs, equivalences, project: none } = p.nodes.get(*input as usize)? else { return None };
            if !none.is_empty() { return None; }
            (inputs, equivalences, project)
        }
        _ => return None,
    };
    (inputs.len() == 2 && decoded(p, defined, inputs, equivalences).is_none()).then_some((inputs.as_slice(), equivalences.as_slice(), project.as_slice()))
}

fn op_at(p: &Program, id: NodeId) -> Result<&Op, EngineError> {
    p.nodes
        .get(id as usize)
        .ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::UnknownNode(id)))
}

/// The inputs `build_node` reads for `op`, in the order they are lowered. `fuse`: the engine's
/// `fuses_join_project`; a projected join reads the join's inputs.
pub fn lowered_inputs<C>(p: &Program, defined: &[(RelId, C)], op: &Op, fuse: bool) -> SmallVec<[NodeId; 2]> {
    if let Some((inputs, ..)) = projected_join(p, defined, op).filter(|_| fuse) {
        return SmallVec::from_slice(inputs);
    }
    match op {
        Op::Get(_) | Op::Delay(_) => SmallVec::new(),
        Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
        | Op::Negate(input) | Op::Reduce { input, .. } | Op::Threshold(input)
        | Op::TopK { input, .. } | Op::Window { input, .. } => smallvec![*input],
        Op::Join { inputs, equivalences, .. } => match decoded(p, defined, inputs, equivalences) {
            Some((side, ..)) => smallvec![inputs[1 - side]],
            None => SmallVec::from_slice(inputs),
        },
        Op::Union(inputs) => SmallVec::from_slice(inputs),
        Op::Antijoin { l, r, .. } => smallvec![*l, *r],
    }
}

/// Roots of the subtrees a LetRec's loop reads that depend on none of its relations (`binds`):
/// independent bodies, and independent inputs of dependent nodes.
pub fn independent_inputs<C>(p: &Program, rec: &LetRec, defined: &[(RelId, C)], fuse: bool) -> std::collections::BTreeSet<NodeId> {
    // Post-order over the nodes the bodies reach, inputs first, with an explicit stack.
    let mut dependent: Vec<Option<bool>> = vec![None; p.nodes.len()];
    let mut stack: Vec<(NodeId, bool)> = rec.bodies.iter().map(|body| (*body, false)).collect();
    while let Some((id, expanded)) = stack.pop() {
        if dependent[id as usize].is_some() { continue; }
        let op = &p.nodes[id as usize];
        let inputs = lowered_inputs(p, defined, op, fuse);
        if expanded {
            let reads_rec = matches!(op, Op::Get(rel) if rec.binds(*rel));
            let value = reads_rec || inputs.iter().any(|input| dependent[*input as usize] == Some(true));
            dependent[id as usize] = Some(value);
            continue;
        }
        stack.push((id, true));
        stack.extend(inputs.iter().filter(|input| dependent[**input as usize].is_none()).map(|input| (*input, false)));
    }
    let mut roots: std::collections::BTreeSet<NodeId> = rec.bodies.iter().copied().filter(|body| dependent[*body as usize] == Some(false)).collect();
    for (id, value) in dependent.iter().enumerate() {
        if *value == Some(true) {
            roots.extend(lowered_inputs(p, defined, &p.nodes[id], fuse).into_iter().filter(|input| dependent[*input as usize] == Some(false)));
        }
    }
    roots
}

/// Lowers every node `id` reaches, inputs first, with an explicit stack; each node is built
/// once and memoized in `nodes`. A node reached again while its inputs are pending is a cycle.
pub fn lower_node<A: Rel>(
    p: &Program,
    a: &mut A,
    nodes: &mut Vec<Option<A::C>>,
    defined: &[(RelId, A::C)],
    id: NodeId,
) -> Result<A::C, EngineError> {
    if let Some(Some(c)) = nodes.get(id as usize) {
        return Ok(c.clone());
    }
    let fuse = a.fuses_join_project();
    let mut stack = vec![(id, lowered_inputs(p, defined, op_at(p, id)?, fuse), 0usize)];
    while let Some(top) = stack.len().checked_sub(1) {
        let (_, inputs, next) = &mut stack[top];
        if let Some(&input) = inputs.get(*next) {
            *next += 1;
            if let Some(Some(_)) = nodes.get(input as usize) {
                continue;
            }
            if stack.iter().any(|(node, ..)| *node == input) {
                return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("node cycle")));
            }
            stack.push((input, lowered_inputs(p, defined, op_at(p, input)?, fuse), 0));
            continue;
        }
        let (node, inputs, _) = stack.pop().expect("a frame on the stack");
        let built: SmallVec<[A::C; 2]> = inputs.iter().map(|n| nodes[*n as usize].clone().expect("input lowered first")).collect();
        let c = build_node(p, a, defined, op_at(p, node)?, &built, fuse)?;
        let c = a.observe(node, c);
        nodes[node as usize] = Some(c.clone());
        if stack.is_empty() {
            return Ok(c);
        }
    }
    unreachable!("the root frame returns")
}

/// One node from its lowered inputs, `built`, in `lowered_inputs` order.
fn build_node<A: Rel>(
    p: &Program,
    a: &mut A,
    defined: &[(RelId, A::C)],
    op: &Op,
    built: &[A::C],
    fuse: bool,
) -> Result<A::C, EngineError> {
    let first = || built[0].clone();
    if let Some((inputs, equivalences, project)) = projected_join(p, defined, op).filter(|_| fuse) {
        let types = inputs.iter().map(|n| a.node_types(p, *n).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join input types")))).collect::<Result<Vec<_>, _>>()?;
        return a.join_project(built.to_vec(), equivalences, &types, project);
    }
    Ok(match op {
        Op::Get(rel) => match defined.iter().find(|(d, _)| d == rel) {
            Some((_, c)) => c.clone(),
            None => a.get(*rel)?,
        },
        Op::Mint { functor, args, .. } => a.mint(first(), *functor, args)?,
        Op::StrCons { mode, .. } => a.str_cons(first(), mode)?,
        Op::Str { op, args, .. } => a.str_op(first(), *op, args)?,
        Op::Mfp { input, filter, map, project } => {
            let types = a.node_types(p, *input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Mfp input types")))?;
            a.mfp(first(), filter, map, project, &types)
        }
        Op::Union(_) => a.union(built.to_vec()),
        Op::Negate(_) => a.negate(first()),
        Op::Join { inputs, equivalences, project } if decoded(p, defined, inputs, equivalences).is_some() => {
            let (side, col, functor) = decoded(p, defined, inputs, equivalences).unwrap();
            let other = inputs[1 - side];
            let types = [other, inputs[side]].map(|n| a.node_types(p, n).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join input types"))));
            let types = [types[0].clone()?, types[1].clone()?];
            let c = a.decode(first(), functor, col, &types)?;
            let (width, ctor) = (types[0].len(), types[1].len());
            let mut all = types[0].clone();
            all.extend(types[1].iter().copied());
            // The decode's columns are the other input's, then the constructor's; the Join's are in
            // input order, then `project` of them.
            let order: Vec<ColId> = if side == 1 {
                (0..width + ctor).map(|x| x as ColId).collect()
            } else {
                (width..width + ctor).chain(0..width).map(|x| x as ColId).collect()
            };
            let project: Vec<ColId> = if project.is_empty() { order } else { project.iter().map(|x| order[*x as usize]).collect() };
            if project.iter().copied().eq((0..width + ctor).map(|x| x as ColId)) {
                c
            } else {
                a.mfp(c, &[], &[], &project, &all)
            }
        }
        Op::Join { inputs, equivalences, project } => {
            let types = inputs.iter().map(|n| a.node_types(p, *n).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join input types")))).collect::<Result<Vec<_>, _>>()?;
            let c = a.join(built.to_vec(), equivalences, &types)?;
            if project.is_empty() {
                c
            } else {
                let all: Vec<Ty> = types.iter().flatten().copied().collect();
                a.mfp(c, &[], &[], project, &all)
            }
        }
        Op::Antijoin { lk, rk, .. } => a.antijoin(built[0].clone(), built[1].clone(), lk, rk),
        Op::Reduce { input, key, aggs } => {
            let types = a.node_types(p, *input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Reduce input types")))?;
            a.reduce(first(), key, aggs, &types)
        }
        Op::Threshold(_) => a.threshold(first()),
        Op::TopK { input, key, order, limit } => {
            let types = a.node_types(p, *input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("TopK input types")))?;
            a.topk(first(), key, order, *limit, &types)?
        }
        Op::Window { input, partition, order, func } => {
            let types = a.node_types(p, *input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Window input types")))?;
            a.window(first(), partition, order, func, &types)?
        }
        Op::Delay(_) => return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Delay"))),
    })
}
