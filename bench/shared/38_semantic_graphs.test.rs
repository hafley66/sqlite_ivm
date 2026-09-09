#[path = "37_semantic_graphs.rs"]
mod semantic_graphs;
use differential_dataflow::input::Input;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use timely::dataflow::operators::probe::Handle;

#[test]
fn literal_integer_semantic_graphs_retract_to_empty() {
    for (family, expected) in [
        ("minmax", vec![vec![0, 2, 2, 2], vec![1, 1, -1, -1]]),
        ("count_distinct", vec![vec![0, 1], vec![1, 1]]),
        ("union_set", vec![vec![0, 2], vec![1, -1], vec![2, 4]]),
        ("except_set", vec![vec![1, -1]]),
        ("intersect_set", vec![vec![0, 2]]),
        ("topk", vec![vec![0, 2], vec![0, 2], vec![1, -1]]),
        (
            "window_rank",
            vec![vec![0, 2, 1], vec![0, 2, 2], vec![1, -1, 1]],
        ),
        ("subquery", vec![vec![0, 2], vec![0, 2]]),
        ("cte", vec![vec![0, 2], vec![0, 2]]),
    ] {
        timely::execute_directly(move |worker| {
            let sink = Arc::new(Mutex::new(BTreeMap::<Vec<i64>, isize>::new()));
            let mut probe = Handle::new();
            let (mut ai, mut bi) = worker.dataflow::<u64, _, _>(|scope| {
                let (ai, a) = scope.new_collection::<[i64; 3], isize>();
                let (bi, b) = scope.new_collection::<[i64; 3], isize>();
                let sink = Arc::clone(&sink);
                semantic_graphs::graph(family, a, b)
                    .consolidate()
                    .inspect(move |(r, _, d)| {
                        let mut s = sink.lock().unwrap();
                        *s.entry(r.clone()).or_default() += d;
                        if s[r] == 0 {
                            s.remove(r);
                        }
                    })
                    .probe_with(&mut probe);
                (ai, bi)
            });
            let a = [[1, 0, 2], [2, 0, 2], [3, 1, -1]];
            let b = [[1, 0, 2], [2, 2, 4]];
            for r in a {
                ai.insert(r);
            }
            for r in b {
                bi.insert(r);
            }
            ai.advance_to(1);
            bi.advance_to(1);
            ai.flush();
            bi.flush();
            while probe.less_than(&1) {
                worker.step();
            }
            let rows: Vec<_> = sink
                .lock()
                .unwrap()
                .iter()
                .flat_map(|(r, n)| {
                    assert!(*n > 0);
                    std::iter::repeat_n(r.clone(), *n as usize)
                })
                .collect();
            assert_eq!(rows, expected, "{family}");
            for r in a {
                ai.remove(r);
            }
            for r in b {
                bi.remove(r);
            }
            ai.advance_to(2);
            bi.advance_to(2);
            ai.flush();
            bi.flush();
            while probe.less_than(&2) {
                worker.step();
            }
            assert!(sink.lock().unwrap().is_empty(), "{family} retract-to-empty");
        });
    }
}
