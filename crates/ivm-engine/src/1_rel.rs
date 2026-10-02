//! The operator algebra each engine implements, and the one lowering written against it.

use ivm_ir::*;
use crate::Host;
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
pub trait Engine: Sized {
    fn install(program: &Program, host: &mut impl Host) -> Result<Self, EngineError>;
    fn settle(&mut self, frontier: Frontier, host: &mut impl Host) -> Result<Delta, EngineError>;
    fn counters(&self) -> Counters;
    fn snapshot(&self, rel: RelId, host: &mut impl Host) -> Result<Vec<(Row, W)>, EngineError>;
    fn intern_snapshot(&self, _functor: RelId, _host: &mut impl Host) -> Result<Vec<(Row, W)>, EngineError> {
        Err(EngineError::new(Stage::Snapshot, None, ErrorKind::Unsupported("intern_snapshot")))
    }
    fn intern_text(&mut self, _text: &str, _host: &mut impl Host) -> Result<Cell, EngineError> {
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("intern_text")))
    }
    /// Interns constructor terms `functor(args)` in order and returns their ids, as a Mint of
    /// the same row would. Each argument is an interned term, a text cell or a raw value.
    /// The constructor's rows hold the new terms from the next settle on.
    fn intern_terms(&mut self, _terms: &[(RelId, Row)], _host: &mut impl Host) -> Result<Vec<Cell>, EngineError> {
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("intern_terms")))
    }
    fn text(&self, _id: Cell, _host: &mut impl Host) -> Result<Option<String>, EngineError> {
        Err(EngineError::new(Stage::Snapshot, None, ErrorKind::Unsupported("text")))
    }
    fn intern_any(&mut self, _value: &ivm_ir::AnyValue, _host: &mut impl Host) -> Result<Cell, EngineError> {
        Err(EngineError::new(Stage::Settle, None, ErrorKind::Unsupported("intern_any")))
    }
    fn any_value(&self, _id: Cell, _host: &mut impl Host) -> Result<ivm_ir::AnyValue, EngineError> {
        Err(EngineError::new(Stage::Snapshot, None, ErrorKind::Unsupported("any_value")))
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
    fn topk(&mut self, _c: Self::C, _key: &[ColId], _order: &[Order], _limit: u32, _input_types: &[Ty]) -> Result<Self::C, EngineError> {
        Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("TopK")))
    }
    fn window(&mut self, _c: Self::C, _partition: &[ColId], _order: &[Order], _func: &WinFn, _input_types: &[Ty]) -> Result<Self::C, EngineError> {
        Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Window")))
    }
    /// Engine-owned fixpoint: returns one collection per `rec.ids`, built with `lower_node` on the engine's inner algebra.
    fn letrec(&mut self, _p: &Program, _rec: &LetRec, _defined: &[(RelId, Self::C)]) -> Result<Vec<Self::C>, EngineError> {
        Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("LetRec")))
    }
    fn output(&mut self, rel: RelId, c: Self::C);
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
    eval_with_text(expr, row, term_lt, &|_| panic!("Text requires a dictionary"), &|| panic!("StrNil requires a dictionary"))
}

pub fn eval_with_text(expr: &Expr, row: &[Cell], term_lt: &dyn Fn(Cell, Cell) -> bool, text: &dyn Fn(u32) -> Cell, nil: &dyn Fn() -> Cell) -> Cell {
    match expr {
        Expr::Col(c) => row[*c as usize],
        Expr::Lit(v) => *v,
        Expr::Text(index) => text(*index),
        Expr::Call(func, args) => {
            let a = |i: usize| eval_with_text(&args[i], row, term_lt, text, nil);
            match func {
                Func::Eq => (a(0) == a(1)) as Cell,
                Func::Ne => (a(0) != a(1)) as Cell,
                Func::Lt => (a(0) < a(1)) as Cell,
                Func::Le => (a(0) <= a(1)) as Cell,
                Func::Gt => (a(0) > a(1)) as Cell,
                Func::Ge => (a(0) >= a(1)) as Cell,
                Func::Add => a(0).wrapping_add(a(1)),
                Func::Sub => a(0).wrapping_sub(a(1)),
                Func::And => (a(0) != 0 && a(1) != 0) as Cell,
                Func::Or => (a(0) != 0 || a(1) != 0) as Cell,
                Func::Not => (a(0) == 0) as Cell,
                Func::TermLt => term_lt(a(0), a(1)) as Cell,
                Func::StrNil => nil(),
            }
        }
    }
}

pub fn eval_typed_with_text(
    expr: &Expr, row: &[Cell], types: &[Ty],
    term_lt: &dyn Fn(Cell, Cell) -> bool,
    text: &dyn Fn(u32) -> Cell, nil: &dyn Fn() -> Cell,
    compare: &dyn Fn(Ty, Cell, Ty, Cell) -> std::cmp::Ordering,
) -> Cell {
    use std::cmp::Ordering;
    match expr {
        Expr::Col(c) => row[*c as usize],
        Expr::Lit(v) => *v,
        Expr::Text(index) => text(*index),
        Expr::Call(Func::StrNil, _) => nil(),
        Expr::Call(func, args) => {
            let a = |i: usize| eval_typed_with_text(&args[i], row, types, term_lt, text, nil, compare);
            let cmp = || compare(expr_type(&args[0], types).unwrap(), a(0), expr_type(&args[1], types).unwrap(), a(1));
            match func {
                Func::Eq => (cmp() == Ordering::Equal) as Cell,
                Func::Ne => (cmp() != Ordering::Equal) as Cell,
                Func::Lt => (cmp() == Ordering::Less) as Cell,
                Func::Le => (cmp() != Ordering::Greater) as Cell,
                Func::Gt => (cmp() == Ordering::Greater) as Cell,
                Func::Ge => (cmp() != Ordering::Less) as Cell,
                Func::Add | Func::Sub if expr_type(expr, types) == Some(Ty::Real) => {
                    let number = |i: usize| if expr_type(&args[i], types) == Some(Ty::Real) {
                        f64::from_bits(a(i) as u64)
                    } else { a(i) as f64 };
                    (if *func == Func::Add { number(0) + number(1) } else { number(0) - number(1) }).to_bits() as i64
                }
                Func::Add => a(0).wrapping_add(a(1)),
                Func::Sub => a(0).wrapping_sub(a(1)),
                Func::And => (a(0) != 0 && a(1) != 0) as Cell,
                Func::Or => (a(0) != 0 || a(1) != 0) as Cell,
                Func::Not => (a(0) == 0) as Cell,
                Func::TermLt => term_lt(a(0), a(1)) as Cell,
                Func::StrNil => unreachable!(),
            }
        }
    }
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
                let cs = a.letrec(p, rec, &defined)?;
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
    let op = p
        .nodes
        .get(id as usize)
        .ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::UnknownNode(id)))?;
    let mut sub = |n: NodeId, a: &mut A| lower_node(p, a, nodes, defined, n);
    let c = match op {
        Op::Get(rel) => match defined.iter().find(|(d, _)| d == rel) {
            Some((_, c)) => c.clone(),
            None => a.get(*rel)?,
        },
        Op::Mint { input, functor, args } => {
            let c = sub(*input, a)?;
            a.mint(c, *functor, args)?
        }
        Op::StrCons { input, mode } => {
            let c = sub(*input, a)?;
            a.str_cons(c, mode)?
        }
        Op::Str { input, op, args } => {
            let c = sub(*input, a)?;
            a.str_op(c, *op, args)?
        }
        Op::Mfp { input, filter, map, project } => {
            let c = sub(*input, a)?;
            let types = p.node_types(*input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Mfp input types")))?;
            a.mfp(c, filter, map, project, &types)
        }
        Op::Union(inputs) => {
            let cs = inputs.iter().map(|n| sub(*n, a)).collect::<Result<Vec<_>, _>>()?;
            a.union(cs)
        }
        Op::Negate(input) => {
            let c = sub(*input, a)?;
            a.negate(c)
        }
        Op::Join { inputs, equivalences } if decoded(p, defined, inputs, equivalences).is_some() => {
            let (side, col, functor) = decoded(p, defined, inputs, equivalences).unwrap();
            let other = inputs[1 - side];
            let types = [other, inputs[side]].map(|n| p.node_types(n).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join input types"))));
            let types = [types[0].clone()?, types[1].clone()?];
            let c = sub(other, a)?;
            let c = a.decode(c, functor, col, &types)?;
            if side == 1 {
                c
            } else {
                // The constructor input came first: its columns lead.
                let (width, ctor) = (types[0].len(), types[1].len());
                let project = (width..width + ctor).chain(0..width).map(|x| x as ColId).collect::<Vec<_>>();
                let mut all = types[0].clone();
                all.extend(types[1].iter().copied());
                a.mfp(c, &[], &[], &project, &all)
            }
        }
        Op::Join { inputs, equivalences } => {
            let cs = inputs.iter().map(|n| sub(*n, a)).collect::<Result<Vec<_>, _>>()?;
            let types = inputs.iter().map(|n| p.node_types(*n).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join input types")))).collect::<Result<Vec<_>, _>>()?;
            a.join(cs, equivalences, &types)?
        }
        Op::Antijoin { l, r, lk, rk } => {
            let l = sub(*l, a)?;
            let r = sub(*r, a)?;
            a.antijoin(l, r, lk, rk)
        }
        Op::Reduce { input, key, aggs } => {
            let c = sub(*input, a)?;
            let types = p.node_types(*input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Reduce input types")))?;
            a.reduce(c, key, aggs, &types)
        }
        Op::Threshold(input) => {
            let c = sub(*input, a)?;
            a.threshold(c)
        }
        Op::TopK { input, key, order, limit } => {
            let c = sub(*input, a)?;
            let types = p.node_types(*input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("TopK input types")))?;
            a.topk(c, key, order, *limit, &types)?
        }
        Op::Window { input, partition, order, func } => {
            let c = sub(*input, a)?;
            let types = p.node_types(*input).ok_or_else(|| EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Window input types")))?;
            a.window(c, partition, order, func, &types)?
        }
        Op::Delay(_) => return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Delay"))),
    };
    let c = a.observe(id, c);
    nodes[id as usize] = Some(c.clone());
    Ok(c)
}
