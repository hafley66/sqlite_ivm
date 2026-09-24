//! Random well-typed programs over the ops `Dd` supports, and set-respecting frontiers over values 0..3.
//! Negate is never generated: SQLite has no EXCEPT ALL to mirror a negative weight.

use super::rng::Rng;
use lab_20260924_0::*;
use std::collections::BTreeSet;

pub const DOMAIN: usize = 3;
/// Widest relation a node may produce.
const WIDE: usize = 4;

struct Gen<'r> {
    rng: &'r mut Rng,
    nodes: Vec<Op>,
    arity: Vec<usize>,
    depth: Vec<usize>,
    /// Relations a `Get` may read: `(id, arity, depth)`.
    rels: Vec<(RelId, usize, usize)>,
}

impl Gen<'_> {
    fn push(&mut self, op: Op, arity: usize, depth: usize) -> NodeId {
        self.nodes.push(op);
        self.arity.push(arity);
        self.depth.push(depth);
        (self.nodes.len() - 1) as NodeId
    }

    /// A node of depth at most `budget` (a derived `Get` counts its body's depth); `op` forbids a bare leaf.
    fn node(&mut self, budget: usize, op: bool) -> NodeId {
        let shared: Vec<NodeId> = (0..self.nodes.len() as NodeId).filter(|n| self.depth[*n as usize] <= budget).collect();
        if !op && !shared.is_empty() && self.rng.chance(10) {
            return shared[self.rng.below(shared.len())];
        }
        if !op && (budget == 0 || self.rng.chance(35)) {
            let fits: Vec<_> = self.rels.iter().copied().filter(|r| r.2 <= budget).collect();
            let (rel, arity, depth) = fits[self.rng.below(fits.len())];
            return self.push(Op::Get(rel), arity, depth);
        }
        let b = budget - 1;
        match self.rng.below(12) {
            0..=2 => self.mfp(b),
            3 | 4 => self.union(b),
            5 | 6 => self.join(b),
            7 | 8 => self.antijoin(b),
            9 | 10 => self.reduce(b),
            _ => {
                let input = self.node(b, false);
                self.push(Op::Threshold(input), self.arity[input as usize], self.depth[input as usize] + 1)
            }
        }
    }

    fn cols(&mut self, n: usize, width: usize) -> Vec<ColId> {
        (0..n).map(|_| self.rng.below(width) as ColId).collect()
    }

    /// Projects `n` to `arity` columns when its width differs.
    fn fit(&mut self, n: NodeId, arity: usize) -> NodeId {
        let have = self.arity[n as usize];
        if have == arity {
            return n;
        }
        let project = self.cols(arity, have);
        let op = Op::Mfp { input: n, filter: vec![], map: vec![], project };
        self.push(op, arity, self.depth[n as usize] + 1)
    }

    fn term(&mut self, width: usize, depth: usize) -> Expr {
        match self.rng.below(if depth == 0 { 2 } else { 5 }) {
            0 => Expr::Col(self.rng.below(width) as ColId),
            1 => Expr::Lit(self.rng.range(0, 4) as Cell - 1),
            2 | 3 => {
                let f = if self.rng.chance(50) { Func::Add } else { Func::Sub };
                Expr::Call(f, vec![self.term(width, depth - 1), self.term(width, depth - 1)])
            }
            _ => self.pred(width, depth - 1),
        }
    }

    fn pred(&mut self, width: usize, depth: usize) -> Expr {
        const CMP: [Func; 6] = [Func::Eq, Func::Ne, Func::Lt, Func::Le, Func::Gt, Func::Ge];
        match self.rng.below(if depth == 0 { 1 } else { 6 }) {
            0 | 1 => Expr::Call(CMP[self.rng.below(6)], vec![self.term(width, 1), self.term(width, 1)]),
            2 => Expr::Call(Func::Not, vec![self.pred(width, depth - 1)]),
            3 => Expr::Call(Func::And, vec![self.pred(width, depth - 1), self.pred(width, depth - 1)]),
            4 => Expr::Call(Func::Or, vec![self.pred(width, depth - 1), self.pred(width, depth - 1)]),
            _ => self.term(width, depth),
        }
    }

    fn mfp(&mut self, b: usize) -> NodeId {
        let input = self.node(b, false);
        let a = self.arity[input as usize];
        let filter = (0..self.rng.range(0, 2)).map(|_| self.pred(a, 2)).collect();
        let map: Vec<Expr> = (0..self.rng.range(0, 2)).map(|j| self.term(a + j, 2)).collect();
        let width = a + map.len();
        let project = if width <= WIDE && self.rng.chance(30) {
            vec![]
        } else {
            let n = self.rng.range(1, width.min(3));
            self.cols(n, width)
        };
        let arity = if project.is_empty() { width } else { project.len() };
        let depth = self.depth[input as usize] + 1;
        self.push(Op::Mfp { input, filter, map, project }, arity, depth)
    }

    fn union(&mut self, b: usize) -> NodeId {
        let first = self.node(b, false);
        let a = self.arity[first as usize];
        let mut inputs = vec![first];
        for _ in 0..self.rng.range(1, 2) {
            let n = self.node(b, false);
            inputs.push(self.fit(n, a));
        }
        let depth = inputs.iter().map(|n| self.depth[*n as usize]).max().unwrap() + 1;
        self.push(Op::Union(inputs), a, depth)
    }

    fn join(&mut self, b: usize) -> NodeId {
        let l = self.node(b, false);
        let l = self.fit(l, self.arity[l as usize].min(WIDE - 1));
        let la = self.arity[l as usize];
        let r = if self.rng.chance(15) { l } else { self.node(b, false) };
        let r = self.fit(r, self.arity[r as usize].min(WIDE - la));
        let ra = self.arity[r as usize];
        let equivalences = (0..self.rng.range(1, 2.min(la).min(ra)))
            .map(|_| vec![(0, self.rng.below(la) as ColId), (1, self.rng.below(ra) as ColId)])
            .collect();
        let depth = self.depth[l as usize].max(self.depth[r as usize]) + 1;
        self.push(Op::Join { inputs: vec![l, r], equivalences }, la + ra, depth)
    }

    fn antijoin(&mut self, b: usize) -> NodeId {
        let l = self.node(b, false);
        let r = self.node(b, false);
        let (la, ra) = (self.arity[l as usize], self.arity[r as usize]);
        let k = if self.rng.chance(10) { 0 } else { self.rng.range(1, 2.min(la).min(ra)) };
        let (lk, rk) = (self.cols(k, la), self.cols(k, ra));
        let depth = self.depth[l as usize].max(self.depth[r as usize]) + 1;
        self.push(Op::Antijoin { l, r, lk, rk }, la, depth)
    }

    fn reduce(&mut self, b: usize) -> NodeId {
        let input = self.node(b, false);
        let a = self.arity[input as usize];
        let mut all: Vec<ColId> = (0..a as ColId).collect();
        self.rng.shuffle(&mut all);
        let key: Vec<ColId> = all[..self.rng.range(0, 2.min(a))].to_vec();
        let aggs: Vec<Agg> = (0..self.rng.range(1, 2))
            .map(|_| {
                let c = self.rng.below(a) as ColId;
                match self.rng.below(4) {
                    0 => Agg::Count,
                    1 => Agg::Sum(c),
                    2 => Agg::Min(c),
                    _ => Agg::Max(c),
                }
            })
            .collect();
        let arity = key.len() + aggs.len();
        let depth = self.depth[input as usize] + 1;
        self.push(Op::Reduce { input, key, aggs }, arity, depth)
    }
}

/// 1-3 sources of arity 1-3, 1-3 derived relations each with body depth budget 1-4, 1-2 outputs.
pub fn program(rng: &mut Rng) -> Program {
    let mut rels = Vec::new();
    let mut gen = Gen { rng, nodes: vec![], arity: vec![], depth: vec![], rels: vec![] };
    let sources = gen.rng.range(1, 3);
    for i in 0..sources {
        let arity = gen.rng.range(1, 3);
        rels.push(Relation { id: i as RelId, name: format!("s{i}"), cols: vec![Ty::Int; arity], kind: RelKind::Source });
        gen.rels.push((i as RelId, arity, 0));
    }
    let mut strata = Vec::new();
    let derived = gen.rng.range(1, 3);
    for k in 0..derived {
        let budget = gen.rng.range(1, 4);
        let body = gen.node(budget, true);
        let (arity, depth) = (gen.arity[body as usize], gen.depth[body as usize]);
        let id = (sources + k) as RelId;
        rels.push(Relation { id, name: format!("d{k}"), cols: vec![Ty::Int; arity], kind: RelKind::Derived });
        strata.push(Stratum::Let { id, body });
        gen.rels.push((id, arity, depth));
    }
    let last = (sources + derived - 1) as RelId;
    let mut outputs = vec![last];
    if derived > 1 && gen.rng.chance(50) {
        outputs.push((sources + gen.rng.below(derived - 1)) as RelId);
    }
    Program { rels, nodes: gen.nodes, strata, outputs }
}

/// 20-50 frontiers of 0-4 changes; an insert only of an absent row, a delete only of a present one.
pub fn frontiers(rng: &mut Rng, p: &Program) -> Vec<Frontier> {
    let sources: Vec<(RelId, usize)> =
        p.rels.iter().filter(|r| r.kind == RelKind::Source).map(|r| (r.id, r.cols.len())).collect();
    let mut state: BTreeSet<(RelId, Row)> = BTreeSet::new();
    let mut out = Vec::new();
    for _ in 0..rng.range(20, 50) {
        let mut changes = Vec::new();
        let n = if rng.chance(10) { 0 } else { rng.range(1, 4) };
        while changes.len() < n {
            let (rel, arity) = sources[rng.below(sources.len())];
            let present: Vec<&Row> = state.iter().filter(|(r, _)| *r == rel).map(|(_, row)| row).collect();
            let row: Row = if !present.is_empty() && rng.chance(40) {
                present[rng.below(present.len())].clone()
            } else {
                (0..arity).map(|_| rng.below(DOMAIN) as Cell).collect()
            };
            let key = (rel, row.clone());
            let w = if state.contains(&key) { -1 } else { 1 };
            if n - changes.len() >= 2 && rng.chance(20) {
                changes.push(SourceChange { rel, row: row.clone(), w });
                changes.push(SourceChange { rel, row, w: -w });
            } else {
                if w > 0 {
                    state.insert(key);
                } else {
                    state.remove(&key);
                }
                changes.push(SourceChange { rel, row, w });
            }
        }
        out.push(Frontier { changes });
    }
    out
}

/// Replays the set contract over a frontier sequence; the shrinker keeps only valid candidates.
pub fn valid(frontiers: &[Frontier]) -> bool {
    let mut state: BTreeSet<(RelId, Row)> = BTreeSet::new();
    frontiers.iter().flat_map(|f| &f.changes).all(|c| {
        let key = (c.rel, c.row.clone());
        if c.w > 0 { state.insert(key) } else { state.remove(&key) }
    })
}
