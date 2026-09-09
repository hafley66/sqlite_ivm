//! DD lowering for the finite integer contracts in 36_semantic_catalog.mjs.
use differential_dataflow::operators::CountTotal;
use differential_dataflow::VecCollection;

pub fn graph<'scope>(
    family: &str,
    a: VecCollection<'scope, u64, [i64; 3]>,
    b: VecCollection<'scope, u64, [i64; 3]>,
) -> VecCollection<'scope, u64, Vec<i64>> {
    let ak = a.clone().map(|[_, k, v]| (k, v));
    let bk = b.map(|[_, k, v]| (k, v));
    match family {
        "minmax" => ak
            .reduce(|_, input, output| {
                let n: isize = input.iter().map(|(_, weight)| *weight).sum();
                assert!(input.iter().all(|(_, weight)| *weight > 0));
                output.push((
                    (
                        n as i64,
                        *input.first().unwrap().0,
                        *input.last().unwrap().0,
                    ),
                    1,
                ));
            })
            .map(|(k, (n, min, max))| vec![k, n, min, max]),
        "count_distinct" => ak
            .distinct()
            .map(|(k, _)| k)
            .count_total()
            .map(|(k, n)| vec![k, n as i64]),
        "union_set" => ak.concat(bk).distinct().map(|(k, v)| vec![k, v]),
        "except_set" => ak
            .distinct()
            .map(|kv| (kv, ()))
            .antijoin(bk.distinct())
            .map(|((k, v), ())| vec![k, v]),
        "intersect_set" => ak
            .distinct()
            .map(|kv| (kv, ()))
            .semijoin(bk.distinct())
            .map(|((k, v), ())| vec![k, v]),
        "topk" => a
            .map(|[id, k, v]| ((), (-v, id, k)))
            .reduce(|_, input, output| {
                // id makes every support unique; retain this ordering through reduce.
                for (row, weight) in input.iter().take(3) {
                    assert_eq!(*weight, 1);
                    output.push((**row, 1));
                }
            })
            .map(|((), (negative_v, _, k))| vec![k, -negative_v]),
        "window_rank" => a
            .map(|[id, k, v]| (k, (v, id)))
            .reduce(|_, input, output| {
                for (index, (row, weight)) in input.iter().enumerate() {
                    assert_eq!(*weight, 1);
                    output.push(((row.0, row.1, index as i64 + 1), 1));
                }
            })
            .map(|(k, (v, _, rank))| vec![k, v, rank]),
        "subquery" | "cte" => ak.filter(|(_, v)| *v >= 0).map(|(k, v)| vec![k, v]),
        _ => panic!("unknown semantic circuit {family}"),
    }
}
