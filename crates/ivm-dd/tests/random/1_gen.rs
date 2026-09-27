//! Random well-typed programs and set-respecting frontiers over a finite key domain.
//! Negate is never generated: SQLite has no EXCEPT ALL to mirror a negative weight.

use super::rng::Rng;
use ivm_dd::*;
use std::collections::BTreeSet;

pub const DOMAIN: usize = 3;
/// Widest relation a node may produce.
const WIDE: usize = 4;

/// Typed source rows use the pre-interned one-character text IDs (empty=1, z=2, a=3, m=4)
/// and IEEE-754 payloads. The two engines receive the same typed cells.
pub fn typed_program(rng: &mut Rng) -> Program {
    let descending = rng.chance(50);
    Program {
        texts: vec!["z".into(), "a".into(), "m".into()],
        rels: vec![
            Relation { id: 0, name: "typed_source".into(), cols: vec![Ty::Text, Ty::Real], kind: RelKind::Source },
            Relation { id: 1, name: "typed_sum".into(), cols: vec![Ty::Text, Ty::Real], kind: RelKind::Derived },
            Relation { id: 2, name: "typed_top".into(), cols: vec![Ty::Text, Ty::Real], kind: RelKind::Derived },
            Relation { id: 3, name: "typed_filter".into(), cols: vec![Ty::Text, Ty::Real], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Reduce { input: 0, key: vec![0], aggs: vec![Agg::Sum(1)] },
            Op::TopK { input: 0, key: vec![], order: vec![Order { col: if rng.chance(50) { 0 } else { 1 }, desc: descending }], limit: 2 },
            Op::Mfp { input: 0, filter: vec![
                Expr::Call(Func::Lt, vec![Expr::Col(0), Expr::Text(0)]),
                Expr::Call(Func::Ge, vec![Expr::Col(1), Expr::Lit(2)]),
            ], map: vec![], project: vec![] },
        ],
        strata: vec![Stratum::Let { id: 1, body: 1 }, Stratum::Let { id: 2, body: 2 }, Stratum::Let { id: 3, body: 3 }],
        outputs: vec![1, 2, 3],
    }
}

pub fn typed_frontiers(rng: &mut Rng) -> Vec<Frontier> {
    let mut live = BTreeSet::new();
    let mut frontiers = Vec::new();
    for _ in 0..12 {
        let text = 2 + rng.below(3) as i64;
        let real = (rng.below(5) as f64 + 0.5).to_bits() as i64;
        let row = vec![text, real];
        let w = if live.remove(&row) { -1 } else { live.insert(row.clone()); 1 };
        frontiers.push(Frontier { changes: vec![SourceChange { rel: 0, row, w }] });
    }
    frontiers
}

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
        match self.rng.below(18) {
            0..=2 => self.mfp(b),
            3 | 4 => self.union(b),
            5 | 6 => self.join(b),
            7 | 8 => self.antijoin(b),
            9 | 10 => self.reduce(b),
            11 | 12 => self.topk(b),
            13..=16 => self.window(b),
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

    fn topk(&mut self, b: usize) -> NodeId {
        let input = self.node(b, false);
        let a = self.arity[input as usize];
        let key_len = if self.rng.chance(25) { 0 } else { self.rng.range(1, a.min(2)) };
        let key = self.cols(key_len, a);
        let order = (0..self.rng.range(1, a.min(2)))
            .map(|_| Order { col: self.rng.below(a) as ColId, desc: self.rng.chance(50) })
            .collect();
        let limit = self.rng.range(1, 3) as u32;
        self.push(Op::TopK { input, key, order, limit }, a, self.depth[input as usize] + 1)
    }

    fn window(&mut self, b: usize) -> NodeId {
        let input = self.node(b, false);
        let a = self.arity[input as usize];
        let partition_len = if self.rng.chance(30) { 0 } else { self.rng.range(1, a.min(2)) };
        let partition = self.cols(partition_len, a);
        let order = (0..self.rng.range(1, a.min(2)))
            .map(|_| Order { col: self.rng.below(a) as ColId, desc: self.rng.chance(50) })
            .collect();
        let func = match self.rng.below(7) {
            0 => WinFn::RowNumber,
            1 => WinFn::Rank,
            2 => WinFn::DenseRank,
            3 => WinFn::Lag(self.rng.range(0, 2) as u32),
            4 => WinFn::Lead(self.rng.range(0, 2) as u32),
            5 => WinFn::Sum(self.rng.below(a) as ColId),
            _ => WinFn::Count,
        };
        self.push(Op::Window { input, partition, order, func }, a + 1, self.depth[input as usize] + 1)
    }
}

/// 1-3 sources of arity 1-3, 1-3 derived relations each with body depth budget 1-4, 1-2 outputs.
pub fn program(rng: &mut Rng) -> Program {
    if rng.chance(50) {
        return recursive_program(rng);
    }
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
    Program { texts: vec![], rels, nodes: gen.nodes, strata, outputs }
}

/// Linear recursion over finite source keys. A tagged SQL CTE can print either one or two
/// mutually recursive unary relations without nesting a recursive table in a subquery.
fn recursive_program(rng: &mut Rng) -> Program {
    let count = rng.range(1, 2);
    let edge = count as RelId;
    let mut rels: Vec<Relation> = (0..count)
        .map(|i| Relation { id: i as RelId, name: format!("seed{i}"), cols: vec![Ty::Id], kind: RelKind::Source })
        .collect();
    rels.push(Relation { id: edge, name: "edge".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Source });
    let ids: Vec<RelId> = (0..count).map(|i| edge + 1 + i as RelId).collect();
    rels.extend(ids.iter().enumerate().map(|(i, id)| Relation {
        id: *id, name: format!("reach{i}"), cols: vec![Ty::Id], kind: RelKind::Derived,
    }));
    let mut gen = Gen { rng, nodes: vec![], arity: vec![], depth: vec![], rels: vec![] };
    for i in 0..count {
        gen.rels.push((i as RelId, 1, 0));
    }
    gen.rels.push((edge, 2, 0));
    let mut bodies = Vec::new();
    for i in 0..count {
        let seed = gen.push(Op::Get(i as RelId), 1, 0);
        let from = gen.push(Op::Get(ids[(i + count - 1) % count]), 1, 0);
        let edges = gen.push(Op::Get(edge), 2, 0);
        let join = gen.push(Op::Join { inputs: vec![from, edges], equivalences: vec![vec![(0, 0), (1, 0)]] }, 3, 1);
        let step = gen.push(Op::Mfp { input: join, filter: vec![], map: vec![], project: vec![2] }, 1, 2);
        bodies.push(gen.push(Op::Union(vec![seed, step]), 1, 3));
    }
    let mut strata = vec![Stratum::LetRec(LetRec { ids: ids.clone(), bodies, limit: None })];
    for id in &ids {
        gen.rels.push((*id, 1, 4));
    }
    // A post-loop stratum exercises TopK over a recursive result or an outer source.
    let input_rel = if gen.rng.chance(50) { ids[gen.rng.below(ids.len())] } else { edge };
    let width = rels.iter().find(|r| r.id == input_rel).unwrap().cols.len();
    let input = gen.push(Op::Get(input_rel), width, 4);
    let key = if width == 2 && gen.rng.chance(50) { vec![0] } else { vec![] };
    let desc = gen.rng.chance(50);
    let limit = gen.rng.range(1, 2) as u32;
    let top = gen.push(Op::TopK { input, key, order: vec![Order { col: (width - 1) as ColId, desc }], limit }, width, 5);
    let top_id = edge + 1 + count as RelId;
    rels.push(Relation { id: top_id, name: "top".into(), cols: vec![Ty::Id; width], kind: RelKind::Derived });
    strata.push(Stratum::Let { id: top_id, body: top });
    let mut outputs = ids;
    outputs.push(top_id);
    Program { texts: vec![], rels, nodes: gen.nodes, strata, outputs }
}

/// Recursive Mint and Antijoin against a lower-stratum blocker, in either order.
pub fn recursive_shapes_program(rng: &mut Rng) -> Program {
    let rels = vec![
        Relation { id: 0, name: "seed0".into(), cols: vec![Ty::Int], kind: RelKind::Source },
        Relation { id: 1, name: "edge".into(), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Source },
        Relation { id: 2, name: "blocked".into(), cols: vec![Ty::Int], kind: RelKind::Source },
        Relation { id: 3, name: "token".into(), cols: vec![Ty::Id, Ty::Int], kind: RelKind::Constructor },
        Relation { id: 4, name: "reach".into(), cols: vec![Ty::Int, Ty::Id], kind: RelKind::Derived },
        Relation { id: 5, name: "reachable".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
    ];
    let mut nodes = vec![
        Op::Get(0),
        Op::Mint { input: 0, functor: 3, args: vec![0] },
        Op::Get(4),
        Op::Get(1),
        Op::Join { inputs: vec![2, 3], equivalences: vec![vec![(0, 0), (1, 0)]] },
        Op::Mfp { input: 4, filter: vec![], map: vec![], project: vec![3] },
        Op::Get(2),
    ];
    let keys = if rng.chance(20) { vec![] } else { vec![0] };
    let step = if rng.chance(50) {
        nodes.push(Op::Antijoin { l: 5, r: 6, lk: keys.clone(), rk: keys });
        nodes.push(Op::Mint { input: 7, functor: 3, args: vec![0] });
        8
    } else {
        nodes.push(Op::Mint { input: 5, functor: 3, args: vec![0] });
        nodes.push(Op::Antijoin { l: 7, r: 6, lk: keys.clone(), rk: keys });
        8
    };
    nodes.push(Op::Union(vec![1, step]));
    nodes.push(Op::Get(4));
    nodes.push(Op::Mfp { input: 10, filter: vec![], map: vec![], project: vec![0] });
    Program {
        texts: vec![],
        rels,
        nodes,
        strata: vec![
            Stratum::LetRec(LetRec { ids: vec![4], bodies: vec![9], limit: None }),
            Stratum::Let { id: 5, body: 11 },
        ],
        outputs: vec![4, 5],
    }
}

/// K5 uses explicit key/cost columns, so the transformation commutes with every operator.
pub fn k5_program() -> Program {
    let rels = vec![
        Relation { id: 0, name: "seed0".into(), cols: vec![Ty::Id], kind: RelKind::Source },
        Relation { id: 1, name: "edge".into(), cols: vec![Ty::Id, Ty::Id], kind: RelKind::Source },
        Relation { id: 2, name: "cost".into(), cols: vec![Ty::Id, Ty::Int], kind: RelKind::Source },
        Relation { id: 3, name: "reach0".into(), cols: vec![Ty::Id], kind: RelKind::Derived },
        Relation { id: 4, name: "best".into(), cols: vec![Ty::Id, Ty::Int], kind: RelKind::Derived },
        Relation { id: 5, name: "total".into(), cols: vec![Ty::Id, Ty::Int, Ty::Int], kind: RelKind::Derived },
    ];
    let nodes = vec![
        Op::Get(0),
        Op::Get(3),
        Op::Get(1),
        Op::Join { inputs: vec![1, 2], equivalences: vec![vec![(0, 0), (1, 0)]] },
        Op::Mfp { input: 3, filter: vec![], map: vec![], project: vec![2] },
        Op::Union(vec![0, 4]),
        Op::Get(3),
        Op::Get(2),
        Op::Join { inputs: vec![6, 7], equivalences: vec![vec![(0, 0), (1, 0)]] },
        Op::Mfp { input: 8, filter: vec![], map: vec![], project: vec![0, 2] },
        Op::TopK { input: 9, key: vec![0], order: vec![Order { col: 1, desc: true }], limit: 1 },
        Op::Get(4),
        Op::Reduce { input: 11, key: vec![0], aggs: vec![Agg::Sum(1), Agg::Count] },
    ];
    let strata = vec![
        Stratum::LetRec(LetRec { ids: vec![3], bodies: vec![5], limit: None }),
        Stratum::Let { id: 4, body: 10 },
        Stratum::Let { id: 5, body: 12 },
    ];
    Program { texts: vec![], rels, nodes, strata, outputs: vec![3, 4, 5] }
}

/// 20-50 frontiers of 0-4 changes; an insert only of an absent row, a delete only of a present one.
pub fn frontiers(rng: &mut Rng, p: &Program) -> Vec<Frontier> {
    let sources: Vec<(RelId, usize)> =
        p.rels.iter().filter(|r| r.kind == RelKind::Source).map(|r| (r.id, r.cols.len())).collect();
    let mut state: BTreeSet<(RelId, Row)> = BTreeSet::new();
    let mut out = Vec::new();
    if p.strata.iter().any(|s| matches!(s, Stratum::LetRec(_))) {
        let edge = p.rels.iter().find(|r| r.name == "edge").unwrap().id;
        let seed = p.rels.iter().find(|r| r.name == "seed0").unwrap().id;
        let cycle = vec![
            SourceChange { rel: seed, row: vec![0], w: 1 },
            SourceChange { rel: edge, row: vec![0, 1], w: 1 },
            SourceChange { rel: edge, row: vec![1, 0], w: 1 },
        ];
        for c in &cycle { state.insert((c.rel, c.row.clone())); }
        out.push(Frontier { changes: cycle });
        state.remove(&(edge, vec![0, 1]));
        out.push(Frontier { changes: vec![SourceChange { rel: edge, row: vec![0, 1], w: -1 }] });
    }
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
