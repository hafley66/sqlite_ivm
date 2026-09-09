//! Independent typed DD graphs for the SQL-facing feature acceptance catalog.
use differential_dataflow::VecCollection;
use std::collections::BTreeSet;
pub type R = (i64, Option<i64>, Option<i64>, String);
pub type Row = Vec<String>;
type D<'s> = VecCollection<'s, u64, R>;
type O<'s> = VecCollection<'s, u64, Row>;
type P<'s> = VecCollection<'s, u64, (Option<R>, Option<R>)>;
pub fn number(v: Option<i64>) -> String {
    v.map(|v| format!("n{v}")).unwrap_or("null".into())
}
pub fn text(v: Option<&str>) -> String {
    v.map(|v| format!("s{v}")).unwrap_or("null".into())
}
fn n(v: i64) -> String {
    number(Some(v))
}
fn id(r: &Option<R>) -> String {
    number(r.as_ref().map(|r| r.0))
}
fn k(r: &Option<R>) -> String {
    number(r.as_ref().and_then(|r| r.1))
}
fn v(r: &Option<R>) -> String {
    number(r.as_ref().and_then(|r| r.2))
}
fn less(a: Option<i64>, b: Option<i64>) -> bool {
    matches!((a,b),(Some(a),Some(b)) if a<b)
}
fn pairs<'s>(a: D<'s>, b: D<'s>, mode: &str, residual: bool, theta: bool) -> P<'s> {
    let ak = a
        .clone()
        .filter(move |r| theta || r.1.is_some())
        .map(move |r| (if theta { None } else { r.1 }, r));
    let bk = b
        .clone()
        .filter(move |r| theta || r.1.is_some())
        .map(move |r| (if theta { None } else { r.1 }, r));
    let matched = ak
        .join(bk)
        .filter(move |(_, (a, b))| !residual || less(a.2, b.2))
        .map(|(_, ab)| ab);
    let mut result = matched.clone().map(|(a, b)| (Some(a), Some(b)));
    if ["left", "full"].contains(&mode) {
        result = result.concat(
            a.map(|r| (r, ()))
                .antijoin(matched.clone().map(|(a, _)| a).distinct())
                .map(|(a, _)| (Some(a), None)),
        );
    }
    if ["right", "full"].contains(&mode) {
        result = result.concat(
            b.map(|r| (r, ()))
                .antijoin(matched.map(|(_, b)| b).distinct())
                .map(|(b, _)| (None, Some(b))),
        );
    }
    result
}
fn top<'s>(
    rows: O<'s>,
    offset: usize,
    limit: usize,
    compare: impl Fn(&Row, &Row) -> std::cmp::Ordering + 'static,
) -> O<'s> {
    rows.map(|r| ((), r))
        .reduce(move |_, input, output| {
            let mut rows = vec![];
            for (r, d) in input {
                assert!(*d > 0);
                for _ in 0..*d {
                    rows.push((*r).clone());
                }
            }
            rows.sort_by(&compare);
            for r in rows.into_iter().skip(offset).take(limit) {
                output.push((r, 1));
            }
        })
        .map(|(_, r)| r)
}
fn numeric(s: &str) -> Option<i64> {
    if s == "null" {
        None
    } else {
        Some(s[1..].parse().unwrap())
    }
}
fn null_last(a: &str, b: &str) -> std::cmp::Ordering {
    match (numeric(a), numeric(b)) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
        (a, b) => a.cmp(&b),
    }
}
fn sum(values: &[i64]) -> String {
    if values.is_empty() {
        number(None)
    } else {
        n(values.iter().sum())
    }
}
fn avg(values: &[i64]) -> String {
    if values.is_empty() {
        number(None)
    } else {
        format!(
            "n{}",
            values.iter().sum::<i64>() as f64 / values.len() as f64
        )
    }
}
pub fn graph<'s>(
    family: &str,
    a: D<'s>,
    b: D<'s>,
    c: D<'s>,
    unit: VecCollection<'s, u64, ()>,
) -> O<'s> {
    match family {
        "right_join" | "full_join" | "left_residual" | "full_residual" => {
            let mode = if family == "right_join" {
                "right"
            } else if family == "left_residual" {
                "left"
            } else {
                "full"
            };
            pairs(a, b, mode, family.ends_with("residual"), false)
                .map(|(a, b)| vec![id(&a), id(&b), k(&a), k(&b), v(&a), v(&b)])
        }
        "theta_join" | "cross_join" | "comma_filter" => pairs(
            a,
            b,
            "inner",
            family != "cross_join",
            family != "comma_filter",
        )
        .map(|(a, b)| vec![id(&a), id(&b)]),
        "using_inner" | "natural_left" => pairs(
            a,
            b,
            if family == "using_inner" {
                "inner"
            } else {
                "left"
            },
            false,
            false,
        )
        .map(|(a, b)| vec![k(&a), id(&a), id(&b)]),
        "using_full" => pairs(a, b, "full", false, false).map(|(a, b)| {
            vec![
                number(
                    a.as_ref()
                        .and_then(|r| r.1)
                        .or(b.as_ref().and_then(|r| r.1)),
                ),
                k(&a),
                k(&b),
                id(&a),
                id(&b),
            ]
        }),
        "using_star" | "qualified_star" => pairs(a, b, "full", false, false).map(|(a, b)| {
            vec![
                id(&a),
                number(
                    a.as_ref()
                        .and_then(|r| r.1)
                        .or(b.as_ref().and_then(|r| r.1)),
                ),
                v(&a),
                text(a.as_ref().map(|r| r.3.as_str())),
                id(&b),
                v(&b),
                text(b.as_ref().map(|r| r.3.as_str())),
            ]
        }),
        "self_outer" => pairs(a.clone(), a, "full", false, false)
            .map(|(a, b)| vec![id(&a), id(&b), k(&a), k(&b)]),
        "outer_chain" => {
            let ab = pairs(a, b, "left", false, false);
            let matched = ab
                .clone()
                .filter(|(_, b)| b.as_ref().and_then(|r| r.1).is_some())
                .map(|ab| (ab.1.as_ref().unwrap().1, ab))
                .join(c.clone().filter(|r| r.1.is_some()).map(|r| (r.1, r)))
                .map(|(_, (ab, c))| (ab, c));
            let result = matched
                .clone()
                .map(|((a, b), c)| vec![id(&a), id(&b), n(c.0), k(&a), k(&b), number(c.1)]);
            let missing_c = ab
                .map(|ab| (ab, ()))
                .antijoin(matched.clone().map(|(ab, _)| ab).distinct())
                .map(|((a, b), _)| vec![id(&a), id(&b), number(None), k(&a), k(&b), number(None)]);
            let missing_ab = c
                .map(|c| (c, ()))
                .antijoin(matched.map(|(_, c)| c).distinct())
                .map(|(c, _)| {
                    vec![
                        number(None),
                        number(None),
                        n(c.0),
                        number(None),
                        number(None),
                        number(c.1),
                    ]
                });
            result.concat(missing_c).concat(missing_ab)
        }
        "filtered_exists" | "filtered_not_exists" | "joined_exists" => {
            let rhs = if family == "joined_exists" {
                b.filter(|r| r.1.is_some())
                    .map(|r| (r.1, r))
                    .semijoin(
                        c.filter(|r| r.2.is_some_and(|v| v > 0) && r.1.is_some())
                            .map(|r| r.1)
                            .distinct(),
                    )
                    .map(|(_, r)| r)
            } else if family == "filtered_exists" {
                b.filter(|r| r.2.is_some_and(|v| v > 0))
            } else {
                b
            };
            let supported = pairs(
                a.clone(),
                rhs,
                "inner",
                family == "filtered_not_exists",
                false,
            )
            .map(|(a, _)| a.unwrap())
            .distinct();
            if family == "filtered_not_exists" {
                a.map(|r| (r, ()))
                    .antijoin(supported)
                    .map(|(a, _)| vec![n(a.0)])
            } else {
                a.map(|r| (r, ()))
                    .semijoin(supported)
                    .map(|(a, _)| vec![n(a.0), number(a.1)])
            }
        }
        "case_cast" => a.map(|r| {
            vec![
                n(r.0),
                text(Some(&match r.2 {
                    Some(v) if v > 0 => v.to_string(),
                    None => r.3,
                    Some(_) => r.3.to_ascii_uppercase(),
                })),
            ]
        }),
        "like" => a
            .filter(|r| r.3.to_ascii_lowercase().starts_with('a'))
            .map(|r| vec![n(r.0), text(Some(&r.3.replace('a', "A")))]),
        "cte_columns" => {
            pairs(a, b, "inner", false, false).map(|(a, b)| vec![id(&a), k(&a), v(&a), v(&b)])
        }
        "cte_shared" => {
            let a = a.filter(|r| r.2.is_some_and(|v| v > 0));
            pairs(a.clone(), a, "inner", false, false).map(|(a, b)| vec![id(&a), id(&b)])
        }
        "group_only" => a.map(|r| vec![number(r.1), text(Some(&r.3))]).distinct(),
        "group_ordinals" => a
            .map(|r| ((r.1, r.3), ()))
            .reduce(|_, input, out| {
                out.push((input.iter().map(|(_, d)| *d as i64).sum::<i64>(), 1))
            })
            .map(|((k, s), count)| vec![number(k), text(Some(&s)), n(count)]),
        "empty_having" => a
            .map(|_| ((), 1i64))
            .concat(unit.map(|_| ((), 0i64)))
            .reduce(|_, input, out| {
                let count = input.iter().map(|(v, d)| **v * (*d as i64)).sum::<i64>();
                if count == 0 {
                    out.push((vec![n(0), number(None)], 1));
                }
            })
            .map(|(_, r)| r),
        "aggregate_limit_zero" => a.filter(|_| false).map(|_| vec![n(0)]),
        "nullable_sum"
        | "aggregate_expression"
        | "having"
        | "having_alias"
        | "aggregate_filter"
        | "distinct_aggs"
        | "group_topk"
        | "group_window"
        | "nested_distinct"
        | "outer_aggregate" => {
            let family = family.to_string();
            let which = family.clone();
            let values = if family == "nested_distinct" {
                a.map(|r| (r.1, r.2))
                    .distinct()
                    .map(|(k, v)| (k, (v, true)))
            } else if family == "outer_aggregate" {
                pairs(a, b, "left", false, false)
                    .map(|(a, b)| (a.unwrap().1, (b.as_ref().and_then(|r| r.2), b.is_some())))
            } else {
                a.map(|r| (r.1, (r.2, true)))
            };
            let grouped = values
                .reduce(move |key, input, out| {
                    let mut values = vec![];
                    let mut count = 0i64;
                    let mut present = 0i64;
                    for ((v, p), d) in input {
                        assert!(*d > 0);
                        count += *d as i64;
                        if *p {
                            present += *d as i64;
                        }
                        if let Some(v) = v {
                            for _ in 0..*d {
                                values.push(*v);
                            }
                        }
                    }
                    let total = values.iter().sum::<i64>();
                    let k = number(*key);
                    let row = match which.as_str() {
                        "nullable_sum" => vec![k, n(count), sum(&values)],
                        "aggregate_expression" => vec![k, n(total + count)],
                        "having" if count <= 1 || total <= 0 => return,
                        "having_alias" if total <= 0 => return,
                        "having" | "having_alias" | "group_topk" | "group_window" => {
                            vec![k, sum(&values)]
                        }
                        "nested_distinct" => vec![k, n(count)],
                        "outer_aggregate" => vec![k, n(present), sum(&values)],
                        "aggregate_filter" => {
                            let positive = values
                                .iter()
                                .copied()
                                .filter(|v| *v > 0)
                                .collect::<Vec<_>>();
                            let negative = values
                                .iter()
                                .copied()
                                .filter(|v| *v < 0)
                                .collect::<Vec<_>>();
                            vec![k, n(positive.len() as i64), sum(&positive), avg(&negative)]
                        }
                        "distinct_aggs" => {
                            let values = values
                                .into_iter()
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect::<Vec<_>>();
                            vec![
                                k,
                                n(values.len() as i64),
                                sum(&values),
                                avg(&values),
                                number(values.first().copied()),
                                number(values.last().copied()),
                            ]
                        }
                        _ => unreachable!(),
                    };
                    out.push((row, 1));
                })
                .map(|(_, r)| r);
            if family == "group_topk" {
                top(grouped, 0, 2, |a, b| {
                    numeric(&b[1])
                        .cmp(&numeric(&a[1]))
                        .then_with(|| null_last(&a[0], &b[0]))
                })
            } else if family == "group_window" {
                grouped
                    .map(|r| ((), r))
                    .reduce(|_, input, out| {
                        let mut rows = input
                            .iter()
                            .map(|(r, d)| {
                                assert_eq!(*d, 1);
                                (*r).clone()
                            })
                            .collect::<Vec<_>>();
                        rows.sort_by(|a, b| {
                            numeric(&b[1])
                                .cmp(&numeric(&a[1]))
                                .then_with(|| numeric(&a[0]).cmp(&numeric(&b[0])))
                        });
                        for (i, mut r) in rows.into_iter().take(2).enumerate() {
                            r.push(n(i as i64 + 1));
                            out.push((r, 1));
                        }
                    })
                    .map(|(_, r)| r)
            } else {
                grouped
            }
        }
        "distinct_topk" | "compound_topk" => {
            let rows = a.map(|r| vec![number(r.1)]);
            let rows = if family == "compound_topk" {
                rows.concat(b.map(|r| vec![number(r.1)]))
            } else {
                rows
            };
            top(rows.distinct(), 1, 3, |a, b| null_last(&a[0], &b[0]))
        }
        "bag_topk" => top(
            a.map(|r| vec![number(r.2)])
                .concat(b.map(|r| vec![number(r.2)])),
            2,
            5,
            |a, b| numeric(&a[0]).cmp(&numeric(&b[0])),
        ),
        "hidden_order" | "ordinal_order" => {
            let ordinal = family == "ordinal_order";
            let result = top(
                a.map(|r| vec![n(r.0), number(r.2)]),
                if ordinal { 0 } else { 1 },
                if ordinal { 2 } else { 3 },
                |a, b| {
                    numeric(&b[1])
                        .cmp(&numeric(&a[1]))
                        .then_with(|| numeric(&a[0]).cmp(&numeric(&b[0])))
                },
            );
            if ordinal {
                result
            } else {
                result.map(|r| vec![r[0].clone()])
            }
        }
        "window_multi" | "window_frame" | "window_named" | "window_topk" => {
            let family = family.to_string();
            let which = family.clone();
            let result = a
                .map(|r| (r.1, (r.2, r.0)))
                .reduce(move |_, input, out| {
                    let mut rows = input
                        .iter()
                        .map(|(r, d)| {
                            assert_eq!(*d, 1);
                            **r
                        })
                        .collect::<Vec<_>>();
                    if ["window_frame", "window_named"].contains(&which.as_str()) {
                        rows.sort_by_key(|r| r.1);
                    }
                    let (mut dense, mut rank) = (0, 0);
                    for (i, (v, id)) in rows.iter().enumerate() {
                        if i == 0 || rows[i - 1].0 != *v {
                            dense += 1;
                            rank = i + 1;
                        }
                        let row = match which.as_str() {
                            "window_multi" => {
                                vec![n(*id), n(i as i64 + 1), n(rank as i64), n(dense)]
                            }
                            "window_topk" => vec![n(*id), n(i as i64 + 1)],
                            "window_named" => {
                                vec![n(*id), number(rows[0].0), number(rows.last().unwrap().0)]
                            }
                            "window_frame" => vec![
                                n(*id),
                                sum(&rows[i.saturating_sub(1)..(i + 2).min(rows.len())]
                                    .iter()
                                    .filter_map(|r| r.0)
                                    .collect::<Vec<_>>()),
                                if i == 0 { n(-9) } else { number(rows[i - 1].0) },
                                number(rows.get(i + 1).and_then(|r| r.0)),
                            ],
                            _ => unreachable!(),
                        };
                        out.push((row, 1));
                    }
                })
                .map(|(_, r)| r);
            if family == "window_topk" {
                top(result, 0, 3, |a, b| {
                    numeric(&a[1])
                        .cmp(&numeric(&b[1]))
                        .then_with(|| numeric(&a[0]).cmp(&numeric(&b[0])))
                })
            } else {
                result
            }
        }
        _ => panic!("missing typed DD feature graph: {family}"),
    }
}
