//! Executable DD contracts independent of the SQL parser and SQL feature catalog.
use differential_dataflow::{
    input::Input,
    operators::{CountTotal, Iterate},
    AsCollection,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Instant,
};
use timely::{
    dataflow::operators::{probe::Handle, vec::unordered_input::UnorderedInput},
    order::Product,
};
type Bag = BTreeMap<Vec<i64>, isize>;
fn add(b: &mut Bag, r: Vec<i64>, d: isize) {
    *b.entry(r.clone()).or_default() += d;
    if b[&r] == 0 {
        b.remove(&r);
    }
}
fn receipt(name: &str, states: usize, start: Instant) {
    println!(
        "{}",
        json!({"engine":"dd","case":name,"status":"ok","states":states,"oracle":"independent finite weighted-map or graph traversal","wall_ms":start.elapsed().as_secs_f64()*1000.0})
    );
}
fn weighted() {
    let start = Instant::now();
    let names = [
        "signed_map_filter_flatmap",
        "signed_concat_negate",
        "weighted_join",
        "shared_arrangement_join",
        "custom_weighted_reduce",
        "count_total",
        "positive_threshold",
    ];
    timely::execute_directly(move |worker| {
        let observed = Arc::new(Mutex::new(BTreeMap::<String, Bag>::new()));
        let mut probe = Handle::new();
        let (mut ai, mut bi) = worker.dataflow::<u64, _, _>(|scope| {
            let (ai, a) = scope.new_collection::<(i64, i64), isize>();
            let (bi, b) = scope.new_collection::<(i64, i64), isize>();
            let aa = a.clone().arrange_by_key();
            let bb = b.clone().arrange_by_key();
            let outputs = [
                a.clone()
                    .filter(|(_, v)| *v >= 0)
                    .flat_map(|(k, v)| [vec![k, v * 2], vec![k, v * 3]]),
                a.clone()
                    .concat(b.clone().negate())
                    .map(|(k, v)| vec![k, v]),
                a.clone().join(b).map(|(k, (x, y))| vec![k, x, y]),
                aa.clone()
                    .join_core(bb.clone(), |k, x, y| Some(vec![*k, *x, *y]))
                    .concat(aa.join_core(bb, |k, x, y| Some(vec![*k, *x, *y]))),
                a.clone()
                    .reduce(|_, input, out| {
                        out.push((
                            (
                                input.iter().map(|(v, d)| **v * *d as i64).sum::<i64>(),
                                input.iter().map(|(_, d)| *d as i64).sum::<i64>(),
                            ),
                            1,
                        ))
                    })
                    .map(|(k, (s, n))| vec![k, s, n]),
                a.clone()
                    .map(|(k, _)| k)
                    .count_total()
                    .map(|(k, n)| vec![k, n as i64]),
                a.threshold(|_, d| if *d > 0 { 1isize } else { 0 })
                    .map(|(k, v)| vec![k, v]),
            ];
            for (name, out) in names.into_iter().zip(outputs) {
                let seen = observed.clone();
                out.consolidate()
                    .inspect(move |(r, _, d)| {
                        add(
                            seen.lock().unwrap().entry(name.into()).or_default(),
                            r.clone(),
                            *d,
                        )
                    })
                    .probe_with(&mut probe);
            }
            (ai, bi)
        });
        let changes: Vec<(Vec<((i64, i64), isize)>, Vec<((i64, i64), isize)>)> = vec![
            (vec![((1, 10), 3), ((2, -4), 1)], vec![((1, 7), 2)]),
            (vec![((1, 10), -1)], vec![((1, 7), -2), ((1, 8), 1)]),
            (vec![((3, 2), -2)], vec![((3, 5), 4)]),
            (vec![((3, 2), 2), ((1, 10), -2)], vec![]),
            (vec![((2, -4), -1)], vec![((1, 8), -1), ((3, 5), -4)]),
        ];
        let (mut a, mut b) = (
            BTreeMap::<(i64, i64), isize>::new(),
            BTreeMap::<(i64, i64), isize>::new(),
        );
        for (step, (da, db)) in changes.into_iter().enumerate() {
            for (r, d) in da {
                ai.update(r, d);
                *a.entry(r).or_default() += d;
            }
            for (r, d) in db {
                bi.update(r, d);
                *b.entry(r).or_default() += d;
            }
            a.retain(|_, d| *d != 0);
            b.retain(|_, d| *d != 0);
            let epoch = step as u64 + 1;
            ai.advance_to(epoch);
            ai.flush();
            bi.advance_to(epoch);
            bi.flush();
            while probe.less_than(&epoch) {
                worker.step();
            }
            let mut expected = vec![Bag::new(); names.len()];
            let mut groups = BTreeMap::<i64, (i64, i64)>::new();
            for (&(k, v), &d) in &a {
                if v >= 0 {
                    add(&mut expected[0], vec![k, v * 2], d);
                    add(&mut expected[0], vec![k, v * 3], d);
                }
                add(&mut expected[1], vec![k, v], d);
                if d > 0 {
                    add(&mut expected[6], vec![k, v], 1);
                }
                let g = groups.entry(k).or_default();
                g.0 += v * d as i64;
                g.1 += d as i64;
                for (&(bk, w), &bd) in &b {
                    if k == bk {
                        add(&mut expected[2], vec![k, v, w], d * bd);
                        add(&mut expected[3], vec![k, v, w], 2 * d * bd);
                    }
                }
            }
            for (&(k, v), &d) in &b {
                add(&mut expected[1], vec![k, v], -d);
            }
            for (k, (s, n)) in groups {
                add(&mut expected[4], vec![k, s, n], 1);
                if n != 0 {
                    add(&mut expected[5], vec![k, n], 1);
                }
            }
            if step == 0 && std::env::args().any(|a| a == "--inject-fault") {
                add(&mut expected[0], vec![999, 999], 1);
            }
            for (name, expected) in names.iter().zip(expected) {
                assert_eq!(
                    observed
                        .lock()
                        .unwrap()
                        .get(*name)
                        .cloned()
                        .unwrap_or_default(),
                    expected,
                    "{name} step {step}"
                );
            }
        }
    });
    for name in names {
        receipt(name, 5, start);
    }
}
fn recursive() {
    let start = Instant::now();
    let names = [
        "binary_transitive_closure",
        "mutual_even_odd_recursion",
        "recursive_min_distance",
        "nested_fixed_points",
        "stratified_antijoin_after_recursion",
    ];
    timely::execute_directly(move |worker| {
        let observed = Arc::new(Mutex::new(BTreeMap::<String, Bag>::new()));
        let mut probe = Handle::new();
        let (mut ei, mut ri, mut bi) = worker.dataflow::<u64, _, _>(|scope| {
            let (ei, e) = scope.new_collection::<(i64, i64), isize>();
            let (ri, r) = scope.new_collection::<i64, isize>();
            let (bi, b) = scope.new_collection::<i64, isize>();
            let edges = e.clone();
            let base = e.clone();
            let closure = e.clone().iterate(move |inner_scope, inner| {
                inner
                    .map(|(s, t)| (t, s))
                    .join(edges.enter(inner_scope))
                    .map(|(_, (s, t))| (s, t))
                    .concat(base.enter(inner_scope))
                    .distinct()
            });
            let roots = r.clone().map(|n| (n, false));
            let seeds = roots.clone();
            let edges = e.clone();
            let parity = roots.iterate(move |inner_scope, inner| {
                inner
                    .join(edges.enter(inner_scope))
                    .map(|(_, (odd, t))| (t, !odd))
                    .concat(seeds.enter(inner_scope))
                    .distinct()
            });
            let roots = r.clone().map(|n| (n, 0i64));
            let seeds = roots.clone();
            let edges = e.clone();
            let distance = roots.iterate(move |inner_scope, inner| {
                inner
                    .join(edges.enter(inner_scope))
                    .map(|(_, (d, t))| (t, d + 1))
                    .concat(seeds.enter(inner_scope))
                    .reduce(|_, input, out| out.push((*input[0].0, 1)))
            });
            let edges = e.clone();
            let seeds = r.clone();
            let nested = r.clone().iterate(move |outer_scope, outer| {
                let entered = edges.enter(outer_scope);
                let fixed = entered.clone();
                let step = entered.clone();
                let closed = entered.iterate(move |inner_scope, inner| {
                    inner
                        .map(|(s, t)| (t, s))
                        .join(step.enter(inner_scope))
                        .map(|(_, (s, t))| (s, t))
                        .concat(fixed.enter(inner_scope))
                        .distinct()
                });
                closed
                    .semijoin(outer)
                    .map(|(_, t)| t)
                    .concat(seeds.enter(outer_scope))
                    .distinct()
            });
            let reached = r
                .clone()
                .concat(closure.clone().semijoin(r).map(|(_, t)| t))
                .distinct();
            let outputs = [
                closure.map(|(s, t)| vec![s, t]),
                parity.map(|(n, p)| vec![n, p as i64]),
                distance.map(|(n, d)| vec![n, d]),
                nested.map(|n| vec![n]),
                reached
                    .map(|n| (n, ()))
                    .antijoin(b.distinct())
                    .map(|(n, _)| vec![n]),
            ];
            for (name, out) in names.into_iter().zip(outputs) {
                let seen = observed.clone();
                out.consolidate()
                    .inspect(move |(r, _, d)| {
                        add(
                            seen.lock().unwrap().entry(name.into()).or_default(),
                            r.clone(),
                            *d,
                        )
                    })
                    .probe_with(&mut probe);
            }
            (ei, ri, bi)
        });
        let states = [
            (
                vec![(1, 2), (2, 3), (3, 2), (1, 4), (4, 3)],
                vec![1],
                vec![],
            ),
            (vec![(1, 2), (3, 2), (1, 4), (4, 3)], vec![1], vec![3]),
            (vec![(1, 2), (3, 2), (1, 4), (4, 3)], vec![], vec![3]),
            (vec![(1, 2), (2, 3), (3, 2)], vec![1], vec![]),
            (vec![], vec![1], vec![]),
        ];
        let (mut olde, mut oldr, mut oldb) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for (step, (edges, roots, blocked)) in states.into_iter().enumerate() {
            let e: BTreeSet<_> = edges.into_iter().collect();
            let r: BTreeSet<_> = roots.into_iter().collect();
            let b: BTreeSet<_> = blocked.into_iter().collect();
            for x in olde.difference(&e) {
                ei.remove(*x);
            }
            for x in e.difference(&olde) {
                ei.insert(*x);
            }
            for x in oldr.difference(&r) {
                ri.remove(*x);
            }
            for x in r.difference(&oldr) {
                ri.insert(*x);
            }
            for x in oldb.difference(&b) {
                bi.remove(*x);
            }
            for x in b.difference(&oldb) {
                bi.insert(*x);
            }
            olde = e.clone();
            oldr = r.clone();
            oldb = b.clone();
            let epoch = step as u64 + 1;
            ei.advance_to(epoch);
            ei.flush();
            ri.advance_to(epoch);
            ri.flush();
            bi.advance_to(epoch);
            bi.flush();
            while probe.less_than(&epoch) {
                worker.step();
            }
            let mut closure = e.clone();
            loop {
                let old = closure.clone();
                for &(s, m) in &old {
                    for &(m2, t) in &e {
                        if m == m2 {
                            closure.insert((s, t));
                        }
                    }
                }
                if old == closure {
                    break;
                }
            }
            let mut parity: BTreeSet<_> = r.iter().map(|n| (*n, false)).collect();
            loop {
                let old = parity.clone();
                for &(n, p) in &old {
                    for &(s, t) in &e {
                        if n == s {
                            parity.insert((t, !p));
                        }
                    }
                }
                if old == parity {
                    break;
                }
            }
            let mut dist: BTreeMap<_, _> = r.iter().map(|n| (*n, 0)).collect();
            loop {
                let old = dist.clone();
                for &(s, t) in &e {
                    if let Some(d) = old.get(&s) {
                        let next = d + 1;
                        let found = dist.entry(t).or_insert(next);
                        *found = (*found).min(next);
                    }
                }
                if old == dist {
                    break;
                }
            }
            let reachable: BTreeSet<_> = dist.keys().copied().collect();
            let rows = [
                closure
                    .iter()
                    .map(|(s, t)| vec![*s, *t])
                    .collect::<Vec<_>>(),
                parity.iter().map(|(n, p)| vec![*n, *p as i64]).collect(),
                dist.iter().map(|(n, d)| vec![*n, *d]).collect(),
                reachable.iter().map(|n| vec![*n]).collect(),
                reachable.difference(&b).map(|n| vec![*n]).collect(),
            ];
            for (name, rows) in names.iter().zip(rows) {
                let expected = rows.into_iter().map(|r| (r, 1)).collect::<Bag>();
                assert_eq!(
                    observed
                        .lock()
                        .unwrap()
                        .get(*name)
                        .cloned()
                        .unwrap_or_default(),
                    expected,
                    "{name} step {step}"
                );
            }
        }
    });
    for name in names {
        receipt(name, 5, start);
    }
}
fn partial_time() {
    let start = Instant::now();
    timely::execute_directly(|worker| {
        let seen = Arc::new(Mutex::new(vec![]));
        let mut probe = Handle::new();
        let (mut ai, mut ac, mut bi, mut bc) = worker.dataflow::<u64, _, _>(|outer| {
            outer.scoped::<Product<u64, u64>, _, _>("partial-order inputs", |scope| {
                let ((ai, ac), a) =
                    scope.new_unordered_input::<((i64, i64), Product<u64, u64>, isize)>();
                let ((bi, bc), b) =
                    scope.new_unordered_input::<((i64, i64), Product<u64, u64>, isize)>();
                let out = seen.clone();
                a.as_collection()
                    .join(b.as_collection())
                    .inspect(move |x| out.lock().unwrap().push(x.clone()))
                    .probe_with(&mut probe);
                (ai, ac, bi, bc)
            })
        });
        let left = Product::new(1, 0);
        let right = Product::new(0, 1);
        let done = Product::new(2, 2);
        assert!(!timely::PartialOrder::less_equal(&left, &right));
        assert!(!timely::PartialOrder::less_equal(&right, &left));
        ai.activate().session(&ac).give(((7, 10), left, 2));
        bi.activate().session(&bc).give(((7, 20), right, 3));
        ac.downgrade(&done);
        for _ in 0..128 {
            worker.step();
        }
        assert!(
            probe.less_than(&done),
            "held B capability must keep join incomplete"
        );
        bc.downgrade(&done);
        while probe.less_than(&done) {
            worker.step();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![((7, (10, 20)), Product::new(1, 1), 6)]
        );
        ai.activate().session(&ac).give(((7, 10), done, -2));
        let end = Product::new(3, 3);
        ac.downgrade(&end);
        bc.downgrade(&end);
        while probe.less_than(&end) {
            worker.step();
        }
        assert_eq!(
            seen.lock().unwrap().last().unwrap(),
            &((7, (10, 20)), done, -6)
        );
    });
    receipt("incomparable_times_join_lub_and_held_frontier", 2, start);
}
fn main() {
    weighted();
    recursive();
    partial_time();
}
