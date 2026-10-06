//! The program as data; nothing here names DD or SQLite.
//! Op names follow Materialize MIR (plans/2026-09-24-ivm-cousins/4_design.md §2.1).

use crate::{StrKind, StrOp};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub type RelId = u32;
pub type NodeId = u32;
pub type ColId = u16;

/// One cell. Int passes through; Text is interned to an i64 id before it reaches an engine.
pub type Cell = i64;
pub type Row = Vec<Cell>;
/// Z-set weight.
pub type W = i64;

/// A native SQLite storage class before or after its engine-specific dictionary encoding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AnyValue {
    Null,
    Integer(i64),
    Real(u64),
    Text(String),
    Blob(Vec<u8>),
}

fn int_real(integer: i64, real: f64) -> Ordering {
    if real.is_nan() { return Ordering::Greater; }
    if real >= 9223372036854775808.0 { return Ordering::Less; }
    if real < i64::MIN as f64 { return Ordering::Greater; }
    integer.cmp(&(real.trunc() as i64)).then_with(|| {
        if real.fract() > 0.0 { Ordering::Less }
        else if real.fract() < 0.0 { Ordering::Greater }
        else { Ordering::Equal }
    })
}

impl AnyValue {
    pub fn sqlite_cmp(&self, other: &Self) -> Ordering {
        fn class(value: &AnyValue) -> u8 {
            match value {
                AnyValue::Null => 0,
                AnyValue::Integer(_) | AnyValue::Real(_) => 1,
                AnyValue::Text(_) => 2,
                AnyValue::Blob(_) => 3,
            }
        }
        class(self).cmp(&class(other)).then_with(|| match (self, other) {
            (AnyValue::Null, AnyValue::Null) => Ordering::Equal,
            (AnyValue::Integer(a), AnyValue::Integer(b)) => a.cmp(b),
            (AnyValue::Real(a), AnyValue::Real(b)) => f64::from_bits(*a).partial_cmp(&f64::from_bits(*b)).unwrap_or(Ordering::Equal),
            (AnyValue::Integer(a), AnyValue::Real(b)) => int_real(*a, f64::from_bits(*b)),
            (AnyValue::Real(a), AnyValue::Integer(b)) => int_real(*b, f64::from_bits(*a)).reverse(),
            (AnyValue::Text(a), AnyValue::Text(b)) => a.cmp(b),
            (AnyValue::Blob(a), AnyValue::Blob(b)) => a.cmp(b),
            _ => Ordering::Equal,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ty {
    Int,
    Id,
    Text,
    Real,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelKind {
    Source,
    Derived,
    /// Persistent dictionary rows `(id, arg0, ...)` for this relation's name.
    Constructor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub id: RelId,
    pub name: String,
    pub cols: Vec<Ty>,
    pub kind: RelKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Func {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    And,
    Or,
    Not,
    /// Structural comparison of two dictionary term IDs.
    TermLt,
    StrNil,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Expr {
    Col(ColId),
    Lit(Cell),
    Text(u32),
    Call(Func, Vec<Expr>),
    /// The id cell of `Program::terms[i]`, interned by the engine at install. Typed `Id`.
    Term(u32),
}

/// One argument of a ground term the engine interns at install.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TermArg {
    /// An earlier entry of `Program::terms`.
    Term(u32),
    /// The text cell of `Program::texts[i]`.
    Text(u32),
    /// A raw cell (an int, a float's bits, an atom's rank).
    Raw(Cell),
}

/// A ground term `functor(args)`: `functor` is a constructor relation; the engine interns it at
/// install, as `Engine::intern_terms` would, and its constructor rows hold it from the first settle.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TermLit {
    pub functor: RelId,
    pub args: Vec<TermArg>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrMode {
    Construct { head: ColId, rest: ColId },
    Decompose { whole: ColId },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Agg {
    Count,
    Sum(ColId),
    Min(ColId),
    Max(ColId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Order {
    pub col: ColId,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WinFn {
    RowNumber,
    Rank,
    DenseRank,
    /// Offset rows back; read the first order column (column 0 if unordered), defaulting to 0.
    Lag(u32),
    /// Offset rows forward; read the first order column (column 0 if unordered), defaulting to 0.
    Lead(u32),
    Sum(ColId),
    Count,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Op {
    Get(RelId),
    /// Intern selected columns under the named constructor relation and append its ID.
    Mint {
        input: NodeId,
        functor: RelId,
        args: Vec<ColId>,
    },
    StrCons { input: NodeId, mode: StrMode },
    /// Apply one string op to `args` columns of each row and append its result: a dictionary
    /// text id for a text result, a raw integer otherwise. A miss drops the row.
    Str {
        input: NodeId,
        op: StrOp,
        args: Vec<ColId>,
    },
    /// Filter, then append `map` columns, then keep `project` (empty = keep all).
    Mfp {
        input: NodeId,
        #[serde(default)]
        filter: Vec<Expr>,
        #[serde(default)]
        map: Vec<Expr>,
        #[serde(default)]
        project: Vec<ColId>,
    },
    Union(Vec<NodeId>),
    Negate(NodeId),
    /// Output columns are the inputs' columns concatenated, then `project` of them (empty = all).
    /// Each equivalence class lists `(input position, column)` pairs that must be equal.
    Join {
        inputs: Vec<NodeId>,
        equivalences: Vec<Vec<(u8, ColId)>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        project: Vec<ColId>,
    },
    Antijoin {
        l: NodeId,
        r: NodeId,
        lk: Vec<ColId>,
        rk: Vec<ColId>,
    },
    /// Output columns: `key` columns, then one per aggregate.
    Reduce {
        input: NodeId,
        key: Vec<ColId>,
        aggs: Vec<Agg>,
    },
    /// Set semantics: accumulated weight > 0 becomes 1, otherwise absent.
    Threshold(NodeId),
    TopK {
        input: NodeId,
        key: Vec<ColId>,
        order: Vec<Order>,
        limit: u32,
    },
    Window {
        input: NodeId,
        partition: Vec<ColId>,
        /// Explicit order defines Rank/DenseRank peers. Other functions break ties by the full row.
        order: Vec<Order>,
        func: WinFn,
    },
    Delay(NodeId),
}

impl Op {
    /// The inputs whose column types `Program::node_types` reads for this node.
    pub fn type_inputs(&self) -> &[NodeId] {
        match self {
            Op::Get(_) => &[],
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Negate(input) | Op::Threshold(input) | Op::Delay(input) | Op::Reduce { input, .. }
            | Op::TopK { input, .. } | Op::Window { input, .. } | Op::Antijoin { l: input, .. } => std::slice::from_ref(input),
            Op::Union(inputs) => &inputs[..inputs.len().min(1)],
            Op::Join { inputs, .. } => inputs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetRec {
    pub ids: Vec<RelId>,
    pub bodies: Vec<NodeId>,
    pub limit: Option<u32>,
    /// Run to their own fixed point inside every round, in order, over the previous round's
    /// values; their ids are visible only inside this LetRec. One level deep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested: Vec<LetRec>,
}

impl LetRec {
    /// `rel` is one of this LetRec's variables or a variable of a LetRec nested in it.
    pub fn binds(&self, rel: RelId) -> bool {
        self.ids.contains(&rel) || self.nested.iter().any(|inner| inner.ids.contains(&rel))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stratum {
    Let { id: RelId, body: NodeId },
    LetRec(LetRec),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    #[serde(default)]
    pub texts: Vec<String>,
    /// Ground terms `Expr::Term` reads, each argument term before the term that names it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<TermLit>,
    pub rels: Vec<Relation>,
    pub nodes: Vec<Op>,
    pub strata: Vec<Stratum>,
    pub outputs: Vec<RelId>,
}

fn reads_strings(e: &Expr) -> bool {
    let mut walk = vec![e];
    while let Some(next) = walk.pop() {
        match next {
            Expr::Text(_) | Expr::Call(Func::StrNil, _) => return true,
            Expr::Call(_, args) => walk.extend(args.iter()),
            _ => {}
        }
    }
    false
}

impl Program {
    pub fn uses_strings(&self) -> bool {
        !self.texts.is_empty() || self.nodes.iter().any(|op| match op {
            Op::StrCons { .. } | Op::Str { .. } => true,
            Op::Mfp { filter, map, .. } => filter.iter().chain(map).any(reads_strings),
            _ => false,
        })
    }

    pub fn rel(&self, id: RelId) -> Option<&Relation> {
        self.rels.iter().find(|rel| rel.id == id)
    }

    /// Explicit stack, post-order over type inputs. A path longer than the node count repeats a
    /// node: that cycle has no types.
    pub fn node_types(&self, id: NodeId) -> Option<Vec<Ty>> {
        enum Step { Enter(NodeId, usize), Build(NodeId) }
        let mut steps = vec![Step::Enter(id, 0)];
        let mut done: Vec<Option<Vec<Ty>>> = Vec::new();
        while let Some(step) = steps.pop() {
            match step {
                Step::Enter(node, depth) => {
                    if depth > self.nodes.len() { return None; }
                    let Some(op) = self.nodes.get(node as usize) else {
                        done.push(None);
                        continue;
                    };
                    steps.push(Step::Build(node));
                    steps.extend(op.type_inputs().iter().rev().map(|n| Step::Enter(*n, depth + 1)));
                }
                Step::Build(node) => {
                    let op = &self.nodes[node as usize];
                    let inputs = op.type_inputs();
                    let base = done.len() - inputs.len();
                    let types = self.op_types(op, |n| done[base + inputs.iter().position(|x| *x == n)?].clone());
                    done.truncate(base);
                    done.push(types);
                }
            }
        }
        done.pop().flatten()
    }

    /// `node_types` for a caller that asks about many nodes of one program: `memo[n]` holds node
    /// `n`'s answer once computed, so shared inputs are typed once across calls.
    pub fn node_types_memo(&self, id: NodeId, memo: &mut Vec<Option<Option<Vec<Ty>>>>) -> Option<Vec<Ty>> {
        if memo.len() < self.nodes.len() {
            memo.resize(self.nodes.len(), None);
        }
        let mut open: Vec<bool> = Vec::new();
        let mut stack = vec![(id, false)];
        while let Some((node, expanded)) = stack.pop() {
            let index = node as usize;
            let op = self.nodes.get(index)?;
            if memo[index].is_some() {
                continue;
            }
            if expanded {
                let types = self.op_types(op, |n| memo.get(n as usize)?.clone()?);
                memo[index] = Some(types);
                continue;
            }
            if open.len() < self.nodes.len() {
                open.resize(self.nodes.len(), false);
            }
            if std::mem::replace(&mut open[index], true) {
                return None;
            }
            stack.push((node, true));
            stack.extend(op.type_inputs().iter().filter(|n| memo.get(**n as usize).is_some_and(Option::is_none)).map(|n| (*n, false)));
        }
        memo.get(id as usize)?.clone()?
    }

    /// One node's column types from its type inputs' types, `types_of(n)`.
    fn op_types(&self, op: &Op, types_of: impl Fn(NodeId) -> Option<Vec<Ty>>) -> Option<Vec<Ty>> {
        Some(match op {
            Op::Get(rel) => self.rel(*rel)?.cols.clone(),
            Op::Mint { input, .. } => {
                let mut cols = types_of(*input)?;
                cols.push(Ty::Id);
                cols
            }
            Op::StrCons { input, mode } => {
                let mut cols = types_of(*input)?;
                cols.extend(std::iter::repeat(Ty::Id).take(match mode { StrMode::Construct { .. } => 1, StrMode::Decompose { .. } => 2 }));
                cols
            }
            Op::Str { input, op, .. } => {
                let mut cols = types_of(*input)?;
                match op.out() { Some(StrKind::Text) => cols.push(Ty::Id), Some(StrKind::Int) => cols.push(Ty::Int), None => {} }
                cols
            }
            Op::Mfp { input, map, project, .. } => {
                let mut cols = types_of(*input)?;
                for expr in map { cols.push(expr_type(expr, &cols)?); }
                if project.is_empty() { cols } else { project.iter().map(|c| cols.get(*c as usize).copied()).collect::<Option<Vec<_>>>()? }
            }
            Op::Union(inputs) => types_of(*inputs.first()?)?,
            Op::Negate(input) | Op::Threshold(input) | Op::Delay(input) => types_of(*input)?,
            Op::Join { inputs, project, .. } => {
                let mut cols = Vec::new();
                for input in inputs { cols.extend(types_of(*input)?); }
                if project.is_empty() { cols } else { project.iter().map(|c| cols.get(*c as usize).copied()).collect::<Option<Vec<_>>>()? }
            }
            Op::Antijoin { l, .. } => types_of(*l)?,
            Op::Reduce { input, key, aggs } => {
                let cols = types_of(*input)?;
                let mut out = key.iter().map(|c| cols.get(*c as usize).copied()).collect::<Option<Vec<_>>>()?;
                out.extend(aggs.iter().map(|agg| match agg {
                    Agg::Min(c) | Agg::Max(c) => cols.get(*c as usize).copied(),
                    Agg::Count => Some(Ty::Int),
                    Agg::Sum(c) => match cols.get(*c as usize)? { Ty::Real => Some(Ty::Real), Ty::Any => Some(Ty::Any), _ => Some(Ty::Int) },
                }).collect::<Option<Vec<_>>>()?);
                out
            }
            Op::TopK { input, .. } => types_of(*input)?,
            Op::Window { input, func, order, .. } => {
                let mut cols = types_of(*input)?;
                let value = order.first().map_or(0, |o| o.col);
                cols.push(match func {
                    WinFn::Sum(c) => cols.get(*c as usize).copied()?,
                    WinFn::Lag(_) | WinFn::Lead(_) => cols.get(value as usize).copied()?,
                    _ => Ty::Int,
                });
                cols
            }
        })
    }
}

/// The type of one node whose `Add`/`Sub` operand types are `operands`.
fn expr_node_type(expr: &Expr, cols: &[Ty], operands: &[Option<Ty>]) -> Option<Ty> {
    match expr {
        Expr::Col(c) => cols.get(*c as usize).copied(),
        Expr::Text(_) | Expr::Call(Func::StrNil, _) => Some(Ty::Text),
        Expr::Lit(_) => Some(Ty::Int),
        Expr::Term(_) => Some(Ty::Id),
        Expr::Call(Func::Add | Func::Sub, _) => match operands {
            [Some(left), Some(right)] => Some(if *left == Ty::Real || *right == Ty::Real { Ty::Real } else { Ty::Int }),
            _ => None,
        },
        Expr::Call(_, _) => Some(Ty::Int),
    }
}

fn type_operands(e: &Expr) -> &[Expr] {
    match e {
        Expr::Call(Func::Add | Func::Sub, args) => &args[..args.len().min(2)],
        _ => &[],
    }
}

/// Only `Add`/`Sub` read their first two operands' types; post-order over those with an
/// explicit stack.
pub fn expr_type(expr: &Expr, cols: &[Ty]) -> Option<Ty> {
    let operands = type_operands(expr);
    if operands.iter().all(|e| type_operands(e).is_empty()) {
        let leaves = [0, 1].map(|i| operands.get(i).and_then(|e| expr_node_type(e, cols, &[])));
        return expr_node_type(expr, cols, &leaves[..operands.len()]);
    }
    let mut order = Vec::new();
    let mut walk = vec![expr];
    while let Some(next) = walk.pop() {
        order.push(next);
        walk.extend(type_operands(next));
    }
    let mut done: Vec<Option<Ty>> = Vec::with_capacity(order.len());
    for e in order.into_iter().rev() {
        let base = done.len() - type_operands(e).len();
        let ty = expr_node_type(e, cols, &done[base..]);
        done.truncate(base);
        done.push(ty);
    }
    done.pop().flatten()
}

/// One signed change to one source row. `w` is +1 or -1 at the raw API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChange {
    pub rel: RelId,
    pub row: Row,
    pub w: W,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    pub changes: Vec<SourceChange>,
}

/// Net signed output changes of one settled frontier, sorted by `(rel, row)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub tick: u64,
    pub changes: Vec<(RelId, Row, W)>,
}
