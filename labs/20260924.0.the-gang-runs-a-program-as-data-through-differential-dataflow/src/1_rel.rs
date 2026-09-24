//! The operator algebra each engine implements, and the one lowering written against it.

use crate::ir::*;
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

pub trait Rel {
    type C: Clone;
    fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError>;
    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C;
    fn union(&mut self, cs: Vec<Self::C>) -> Self::C;
    fn negate(&mut self, c: Self::C) -> Self::C;
    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>]) -> Result<Self::C, EngineError>;
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C;
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg]) -> Self::C;
    fn threshold(&mut self, c: Self::C) -> Self::C;
    fn output(&mut self, rel: RelId, c: Self::C);
}

/// Scalar semantics shared by every engine: comparisons and logic yield 0 or 1.
pub fn eval(expr: &Expr, row: &[Cell]) -> Cell {
    match expr {
        Expr::Col(c) => row[*c as usize],
        Expr::Lit(v) => *v,
        Expr::Call(func, args) => {
            let a = |i: usize| eval(&args[i], row);
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
                let c = node(p, a, &mut nodes, &defined, *body)?;
                defined.push((*id, c));
            }
            Stratum::LetRec(_) => {
                return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("LetRec")))
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

fn node<A: Rel>(
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
    let mut sub = |n: NodeId, a: &mut A| node(p, a, nodes, defined, n);
    let c = match op {
        Op::Get(rel) => match defined.iter().find(|(d, _)| d == rel) {
            Some((_, c)) => c.clone(),
            None => a.get(*rel)?,
        },
        Op::Mfp { input, filter, map, project } => {
            let c = sub(*input, a)?;
            a.mfp(c, filter, map, project)
        }
        Op::Union(inputs) => {
            let cs = inputs.iter().map(|n| sub(*n, a)).collect::<Result<Vec<_>, _>>()?;
            a.union(cs)
        }
        Op::Negate(input) => {
            let c = sub(*input, a)?;
            a.negate(c)
        }
        Op::Join { inputs, equivalences } => {
            let cs = inputs.iter().map(|n| sub(*n, a)).collect::<Result<Vec<_>, _>>()?;
            a.join(cs, equivalences)?
        }
        Op::Antijoin { l, r, lk, rk } => {
            let l = sub(*l, a)?;
            let r = sub(*r, a)?;
            a.antijoin(l, r, lk, rk)
        }
        Op::Reduce { input, key, aggs } => {
            let c = sub(*input, a)?;
            a.reduce(c, key, aggs)
        }
        Op::Threshold(input) => {
            let c = sub(*input, a)?;
            a.threshold(c)
        }
        Op::TopK { .. } => return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("TopK"))),
        Op::Window { .. } => return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Window"))),
        Op::Delay(_) => return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Delay"))),
    };
    nodes[id as usize] = Some(c.clone());
    Ok(c)
}
