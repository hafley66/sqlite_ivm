//! Metamorphic checks, engine against itself: K4 ids, K5 values, K7 frontier split.

use super::drive::{raw, snapshot, Case};
use super::{gen, rng::Rng};
use ivm_dd::*;
use std::collections::BTreeMap;

/// Fresh sparse RelIds, shuffled NodeIds, renamed relations; strata keep their dependency order.
fn permuted(p: &Program, rng: &mut Rng) -> (Program, BTreeMap<RelId, RelId>) {
    let mut ids: Vec<RelId> = (0..(p.rels.len() * 4) as RelId).collect();
    rng.shuffle(&mut ids);
    let rel: BTreeMap<RelId, RelId> = p.rels.iter().zip(&ids).map(|(r, id)| (r.id, *id)).collect();
    let mut perm: Vec<NodeId> = (0..p.nodes.len() as NodeId).collect();
    rng.shuffle(&mut perm);
    let n = |id: &NodeId| perm[*id as usize];
    let mut nodes = vec![Op::Get(0); p.nodes.len()];
    for (old, op) in p.nodes.iter().enumerate() {
        nodes[perm[old] as usize] = match op.clone() {
            Op::Get(r) => Op::Get(rel[&r]),
            Op::Mfp { input, filter, map, project } => Op::Mfp { input: n(&input), filter, map, project },
            Op::Union(ns) => Op::Union(ns.iter().map(n).collect()),
            Op::Negate(i) => Op::Negate(n(&i)),
            Op::Join { inputs, equivalences } => Op::Join { inputs: inputs.iter().map(n).collect(), equivalences },
            Op::Antijoin { l, r, lk, rk } => Op::Antijoin { l: n(&l), r: n(&r), lk, rk },
            Op::Reduce { input, key, aggs } => Op::Reduce { input: n(&input), key, aggs },
            Op::Threshold(i) => Op::Threshold(n(&i)),
            Op::TopK { input, key, order, limit } => Op::TopK { input: n(&input), key, order, limit },
            Op::Window { input, partition, order, func } => Op::Window { input: n(&input), partition, order, func },
            Op::Delay(input) => Op::Delay(n(&input)),
            op => panic!("permute: unsupported {op:?}"),
        };
    }
    let mut rels: Vec<Relation> =
        p.rels.iter().map(|r| Relation { id: rel[&r.id], name: format!("q{}", rel[&r.id]), ..r.clone() }).collect();
    rng.shuffle(&mut rels);
    let strata = p
        .strata
        .iter()
        .map(|s| match s {
            Stratum::Let { id, body } => Stratum::Let { id: rel[id], body: n(body) },
            Stratum::LetRec(rec) => Stratum::LetRec(LetRec {
                ids: rec.ids.iter().map(|id| rel[id]).collect(),
                bodies: rec.bodies.iter().map(n).collect(),
                limit: rec.limit,
                nested: rec.nested.iter().map(|inner| LetRec {
                    ids: inner.ids.iter().map(|id| rel[id]).collect(),
                    bodies: inner.bodies.iter().map(n).collect(),
                    limit: inner.limit,
                    nested: Vec::new(),
                }).collect(),
            }),
        })
        .collect();
    let mut outputs: Vec<RelId> = p.outputs.iter().map(|o| rel[o]).collect();
    rng.shuffle(&mut outputs);
    (Program { terms: vec![], texts: p.texts.clone(), rels, nodes, strata, outputs }, rel)
}

fn settle<E: Engine>(e: &mut E, f: &Frontier, at: usize) -> Result<Vec<(RelId, Row, W)>, String> {
    let d = e.settle(f.clone()).map_err(|e| format!("frontier {at}: settle: {e}"))?;
    raw(&d).map_err(|e| format!("frontier {at}: {e}"))?;
    Ok(d.changes)
}

/// K4: the permuted program's deltas and snapshots equal the original's under the RelId map.
pub fn permute<E: Engine>(case: &Case) -> Result<(), String> {
    let mut rng = Rng(case.seed ^ 0x9e4d_0001);
    let p = &case.program;
    let (q, map) = permuted(p, &mut rng);
    let mut a = E::install(p).map_err(|e| format!("install: {e}"))?;
    let mut b = E::install(&q).map_err(|e| format!("install permuted: {e}"))?;
    for (i, f) in case.frontiers.iter().enumerate() {
        let g = Frontier { changes: f.changes.iter().map(|c| SourceChange { rel: map[&c.rel], ..c.clone() }).collect() };
        let da = settle(&mut a, f, i)?;
        let db = settle(&mut b, &g, i)?;
        let mut want: Vec<(RelId, Row, W)> = da.into_iter().map(|(r, row, w)| (map[&r], row, w)).collect();
        want.sort();
        if db != want {
            return Err(format!("frontier {i}: permuted delta\n  expected {want:?}\n  got      {db:?}"));
        }
        for out in &p.outputs {
            let (sa, sb) = (snapshot(&a, *out)?, snapshot(&b, map[out])?);
            if sa != sb {
                return Err(format!("frontier {i}: permuted snapshot of rel {out}\n  expected {sa:?}\n  got      {sb:?}"));
            }
        }
    }
    Ok(())
}

/// K5's fixed bijection on the generated key domain and positive cost scale.
/// The final Count column stays unchanged; the Sum column scales with costs.
fn k5_row(p: &Program, rel: RelId, row: &Row) -> Row {
    const KEY: [Cell; gen::DOMAIN] = [2, 0, 1];
    let name = p.rel(rel).unwrap().name.as_str();
    row.iter().enumerate().map(|(i, value)| {
        match (name, i) {
            ("total", 2) => *value,
            ("cost" | "best" | "total", 1) => value * 3,
            _ => KEY[*value as usize],
        }
    }).collect()
}

/// K5: transform every source frontier, then compare transformed deltas and snapshots.
pub fn values<E: Engine>(case: &Case) -> Result<(), String> {
    let p = &case.program;
    let mut a = E::install(p).map_err(|e| format!("install: {e}"))?;
    let mut b = E::install(p).map_err(|e| format!("install transformed: {e}"))?;
    for (i, f) in case.frontiers.iter().enumerate() {
        let transformed = Frontier { changes: f.changes.iter().map(|c| SourceChange {
            rel: c.rel, row: k5_row(p, c.rel, &c.row), w: c.w,
        }).collect() };
        let da = settle(&mut a, f, i)?;
        let db = settle(&mut b, &transformed, i)?;
        let mut want: Vec<_> = da.into_iter().map(|(rel, row, w)| (rel, k5_row(p, rel, &row), w)).collect();
        want.sort();
        if db != want {
            return Err(format!("frontier {i}: K5 delta\n  expected {want:?}\n  got      {db:?}"));
        }
        for out in &p.outputs {
            let mut want: Vec<_> = snapshot(&a, *out)?.into_iter()
                .map(|(row, w)| (k5_row(p, *out, &row), w)).collect();
            want.sort();
            let got = snapshot(&b, *out)?;
            if got != want {
                return Err(format!("frontier {i}: K5 snapshot of rel {out}\n  expected {want:?}\n  got      {got:?}"));
            }
        }
    }
    Ok(())
}

/// K7: every frontier of 2+ changes is cut into 2..=n contiguous chunks; contiguous chunks of a valid
/// frontier keep every intermediate state valid, and the consolidated chunk deltas must equal the whole.
pub fn split<E: Engine>(case: &Case) -> Result<(), String> {
    let mut rng = Rng(case.seed ^ 0x5b17_0002);
    let p = &case.program;
    let mut a = E::install(p).map_err(|e| format!("install: {e}"))?;
    let mut b = E::install(p).map_err(|e| format!("install split: {e}"))?;
    let mut chunked = Vec::new();
    for (i, f) in case.frontiers.iter().enumerate() {
        let whole = settle(&mut a, f, i)?;
        let n = f.changes.len();
        let mut cuts: Vec<usize> = (1..n).filter(|_| rng.chance(50)).collect();
        if cuts.is_empty() && n >= 2 {
            cuts.push(rng.range(1, n - 1));
        }
        cuts.insert(0, 0);
        cuts.push(n);
        let mut sum: BTreeMap<(RelId, Row), W> = BTreeMap::new();
        for w in cuts.windows(2) {
            let chunk = Frontier { changes: f.changes[w[0]..w[1]].to_vec() };
            for (rel, row, w) in settle(&mut b, &chunk, i)? {
                *sum.entry((rel, row)).or_default() += w;
            }
            chunked.push(chunk);
        }
        if !gen::valid(&chunked) {
            return Err(format!("frontier {i}: split broke the set contract"));
        }
        let parts: Vec<(RelId, Row, W)> = sum.into_iter().filter(|(_, w)| *w != 0).map(|((r, row), w)| (r, row, w)).collect();
        if parts != whole {
            return Err(format!("frontier {i}: split at {cuts:?}\n  whole  {whole:?}\n  chunks {parts:?}"));
        }
        for out in &p.outputs {
            let (sa, sb) = (snapshot(&a, *out)?, snapshot(&b, *out)?);
            if sa != sb {
                return Err(format!("frontier {i}: split snapshot of rel {out}\n  whole  {sa:?}\n  chunks {sb:?}"));
            }
        }
    }
    Ok(())
}
