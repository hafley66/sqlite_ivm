use ivm_engine::{EngineError, ErrorKind, Stage};
use ivm_ir::*;

pub fn cols(n: usize, prefix: &str) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}c{i}")).collect()
}
pub fn select(n: usize, prefix: &str) -> String {
    cols(n, prefix)
        .into_iter()
        .chain([format!("{prefix}w")])
        .collect::<Vec<_>>()
        .join(",")
}
pub fn decl(n: usize) -> String {
    cols(n, "")
        .into_iter()
        .map(|c| format!("{c} BIGINT NOT NULL"))
        .chain(["w BIGINT NOT NULL".into()])
        .collect::<Vec<_>>()
        .join(",")
}
pub fn bag_select(n: usize, prefix: &str) -> String {
    if n == 0 {
        format!("{prefix}unit")
    } else {
        cols(n, prefix).join(",")
    }
}
pub fn bag_decl(n: usize) -> String {
    if n == 0 {
        "unit BIGINT NOT NULL".into()
    } else {
        cols(n, "")
            .iter()
            .map(|c| format!("{c} BIGINT NOT NULL"))
            .collect::<Vec<_>>()
            .join(",")
    }
}
pub fn counted(table: &str, n: usize) -> String {
    let cs = cols(n, "").join(",");
    if n == 0 {
        format!("SELECT count(*)::BIGINT AS w FROM {table} HAVING count(*)>0")
    } else {
        format!("SELECT {cs},count(*)::BIGINT AS w FROM {table} GROUP BY {cs}")
    }
}
pub fn inputs(op: &Op) -> Vec<NodeId> {
    match op {
        Op::Get(_) => vec![],
        Op::Join { inputs, .. } | Op::Union(inputs) => inputs.clone(),
        Op::Antijoin { l, r, .. } => vec![*l, *r],
        _ => op.type_inputs().to_vec(),
    }
}
pub fn operator(op: &Op) -> &'static str {
    match op {
        Op::Get(_) => "Get",
        Op::Mfp { .. } => "Mfp",
        Op::Union(_) => "Union",
        Op::Negate(_) => "Negate",
        Op::Threshold(_) => "Threshold",
        Op::Join { .. } => "Join",
        Op::Antijoin { .. } => "Antijoin",
        Op::Reduce { .. } => "Reduce",
        Op::TopK { .. } => "TopK",
        Op::Window { .. } => "Window",
        Op::Mint { .. } => "Mint",
        Op::Str { .. } => "Str",
        Op::StrCons { .. } => "StrCons",
        Op::Delay(_) => "Delay",
    }
}
pub fn consolidate(query: &str, n: usize) -> String {
    let c = cols(n, "");
    let group = if n == 0 {
        String::new()
    } else {
        format!("GROUP BY {}", c.join(","))
    };
    let head = if n == 0 {
        String::new()
    } else {
        format!("{},", c.join(","))
    };
    format!("SELECT {head}sum(w)::BIGINT AS w FROM ({query}) q {group} HAVING sum(w)<>0")
}
pub fn ordered(ty: Ty, cell: &str) -> Result<String, EngineError> {
    Ok(match ty {
        Ty::Int => cell.into(),
        Ty::Text => format!("(SELECT value FROM texts WHERE id={cell})"),
        Ty::Id => format!("coalesce((SELECT sortkey FROM dict WHERE id={cell}), '00' || lpad(to_hex(({cell})::HUGEINT+9223372036854775808),16,'0'))"),
        Ty::Real | Ty::Any => return Err(EngineError::new(Stage::Install,None,ErrorKind::Unsupported("ordered Real/Any SQL cell conversion"))),
    })
}
fn expr(
    e: &Expr,
    columns: &[String],
    types: &[Ty],
    texts: &[Cell],
    terms: &[Cell],
    nil: Cell,
) -> Result<(String, Ty), EngineError> {
    let bad = || {
        EngineError::new(
            Stage::Install,
            None,
            ErrorKind::Unsupported("invalid scalar expression"),
        )
    };
    Ok(match e {
        Expr::Col(c) => (
            columns.get(*c as usize).ok_or_else(bad)?.clone(),
            *types.get(*c as usize).ok_or_else(bad)?,
        ),
        Expr::Lit(n) => (format!("({n})::BIGINT"), Ty::Int),
        Expr::Text(t) => (
            texts.get(*t as usize).ok_or_else(bad)?.to_string(),
            Ty::Text,
        ),
        Expr::Term(t) => (terms.get(*t as usize).ok_or_else(bad)?.to_string(), Ty::Id),
        Expr::Call(Func::StrNil, args) => {
            if !args.is_empty() {
                return Err(bad());
            }
            (nil.to_string(), Ty::Text)
        }
        Expr::Call(f, args) => {
            if args.len() != if *f == Func::Not { 1 } else { 2 } {
                return Err(bad());
            }
            let a = args
                .iter()
                .map(|x| expr(x, columns, types, texts, terms, nil))
                .collect::<Result<Vec<_>, _>>()?;
            let (x, xt) = a.first().ok_or_else(bad)?;
            if *f == Func::Not {
                return Ok((format!("(({x})=0)::BIGINT"), Ty::Int));
            }
            let (y, yt) = a.get(1).ok_or_else(bad)?;
            let body = match f {
                Func::Add | Func::Sub => {
                    if matches!(xt, Ty::Real | Ty::Any) || matches!(yt, Ty::Real | Ty::Any) {
                        return Err(EngineError::new(
                            Stage::Install,
                            None,
                            ErrorKind::Unsupported("arithmetic Real/Any SQL cell conversion"),
                        ));
                    }
                    let op = if *f == Func::Add { "+" } else { "-" };
                    return Ok((format!("((((({x})::HUGEINT {op} ({y})::HUGEINT)+9223372036854775808)%18446744073709551616+18446744073709551616)%18446744073709551616-9223372036854775808)::BIGINT"), Ty::Int));
                }
                Func::And => format!("({x})<>0 AND ({y})<>0"),
                Func::Or => format!("({x})<>0 OR ({y})<>0"),
                _ => {
                    let op = match f {
                        Func::Eq => "=",
                        Func::Ne => "<>",
                        Func::Lt | Func::TermLt => "<",
                        Func::Le => "<=",
                        Func::Gt => ">",
                        Func::Ge => ">=",
                        _ => return Err(bad()),
                    };
                    if *f == Func::TermLt {
                        return Ok((
                            format!("({}<{})::BIGINT", ordered(Ty::Id, x)?, ordered(Ty::Id, y)?),
                            Ty::Int,
                        ));
                    }
                    let rank = |t: Ty| match t {
                        Ty::Int | Ty::Real => 1,
                        Ty::Text => 2,
                        Ty::Id => 3,
                        Ty::Any => 4,
                    };
                    if rank(*xt) != rank(*yt) {
                        return Ok((
                            format!("({} {op} {})::BIGINT", rank(*xt), rank(*yt)),
                            Ty::Int,
                        ));
                    }
                    let direct = matches!(f, Func::Eq | Func::Ne)
                        && xt == yt
                        && !matches!(xt, Ty::Real | Ty::Any);
                    let left = if direct { x.clone() } else { ordered(*xt, x)? };
                    let right = if direct { y.clone() } else { ordered(*yt, y)? };
                    format!("({left}) {op} ({right})")
                }
            };
            (format!("({body})::BIGINT"), Ty::Int)
        }
    })
}
pub fn query(
    p: &Program,
    id: NodeId,
    types: &[Vec<Ty>],
    texts: &[Cell],
    terms: &[Cell],
    nil: Cell,
    bag: bool,
) -> Result<Option<String>, EngineError> {
    let unsupported = |name| EngineError::new(Stage::Install, None, ErrorKind::Unsupported(name));
    let op = &p.nodes[id as usize];
    let width = types[id as usize].len();
    let np = if bag { "n" } else { "zn" };
    let rp = if bag { "r" } else { "zr" };
    let q = match op {
        Op::Get(r) => format!("SELECT * FROM {rp}{r}"),
        Op::Mint { .. } | Op::Str { .. } | Op::StrCons { .. } => return Ok(None),
        Op::Delay(_) => return Err(unsupported("Delay")),
        Op::Mfp {
            input,
            filter,
            map,
            project,
        } => {
            let mut ts = types[*input as usize].clone();
            let mut cs = cols(ts.len(), "s.");
            let predicates = filter
                .iter()
                .map(|e| expr(e, &cs, &ts, texts, terms, nil).map(|(v, _)| format!("({v})<>0")))
                .collect::<Result<Vec<_>, _>>()?;
            for m in map {
                let (v, t) = expr(m, &cs, &ts, texts, terms, nil)?;
                cs.push(v);
                ts.push(t);
            }
            let indices: Vec<_> = if project.is_empty() {
                (0..cs.len()).collect()
            } else {
                project.iter().map(|i| *i as usize).collect()
            };
            let mut out = indices
                .iter()
                .enumerate()
                .map(|(i, j)| {
                    cs.get(*j)
                        .map(|c| format!("{c} AS c{i}"))
                        .ok_or_else(|| unsupported("Mfp project column"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !bag {
                out.push("s.w".into());
            } else if out.is_empty() {
                out.push("1::BIGINT AS unit".into());
            }
            format!(
                "SELECT {} FROM {np}{input} s {}",
                out.join(","),
                if predicates.is_empty() {
                    String::new()
                } else {
                    format!("WHERE {}", predicates.join(" AND "))
                }
            )
        }
        Op::Union(inputs) => inputs
            .iter()
            .map(|n| format!("SELECT * FROM {np}{n}"))
            .collect::<Vec<_>>()
            .join(" UNION ALL "),
        Op::Negate(input) => format!(
            "SELECT {}-w AS w FROM {np}{input}",
            if width == 0 {
                String::new()
            } else {
                format!("{},", cols(width, "").join(","))
            }
        ),
        Op::Threshold(input) if bag => {
            format!("SELECT DISTINCT {} FROM {np}{input}", bag_select(width, ""))
        }
        Op::Threshold(input) => format!(
            "SELECT {}1::BIGINT AS w FROM {np}{input} WHERE w>0",
            if width == 0 {
                String::new()
            } else {
                format!("{},", cols(width, "").join(","))
            }
        ),
        Op::Join {
            inputs,
            equivalences,
            project,
        } => {
            for (side, col) in equivalences.iter().flatten() {
                let input = *inputs
                    .get(*side as usize)
                    .ok_or_else(|| unsupported("Join input index"))?;
                let ty = *types[input as usize]
                    .get(*col as usize)
                    .ok_or_else(|| unsupported("Join column index"))?;
                if matches!(ty, Ty::Real | Ty::Any) {
                    return Err(unsupported("Join Real/Any SQL equality"));
                }
            }
            let mut all = Vec::new();
            for (i, n) in inputs.iter().enumerate() {
                all.extend(cols(types[*n as usize].len(), &format!("s{i}.")));
            }
            let selected = if project.is_empty() {
                all.iter().collect::<Vec<_>>()
            } else {
                project
                    .iter()
                    .map(|c| {
                        all.get(*c as usize)
                            .ok_or_else(|| unsupported("Join projection column index"))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut out = selected
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c} AS c{i}"))
                .collect::<Vec<_>>();
            if !bag {
                out.push(format!(
                    "{} AS w",
                    (0..inputs.len())
                        .map(|i| format!("s{i}.w"))
                        .collect::<Vec<_>>()
                        .join("*")
                ));
            } else if out.is_empty() {
                out.push("1::BIGINT AS unit".into());
            }
            let from = inputs
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{np}{n} s{i}"))
                .collect::<Vec<_>>()
                .join(" CROSS JOIN ");
            let eq = equivalences
                .iter()
                .flat_map(|e| {
                    e.windows(2).map(|pair| {
                        format!(
                            "s{}.c{}=s{}.c{}",
                            pair[0].0, pair[0].1, pair[1].0, pair[1].1
                        )
                    })
                })
                .collect::<Vec<_>>();
            format!(
                "SELECT {} FROM {from} {}",
                out.join(","),
                if eq.is_empty() {
                    String::new()
                } else {
                    format!("WHERE {}", eq.join(" AND "))
                }
            )
        }
        Op::Antijoin { l, r, lk, rk } => {
            if lk.len() != rk.len() {
                return Err(unsupported("Antijoin key arity"));
            }
            for (input, key) in [(*l, lk), (*r, rk)] {
                for col in key {
                    let ty = *types[input as usize]
                        .get(*col as usize)
                        .ok_or_else(|| unsupported("Antijoin column index"))?;
                    if matches!(ty, Ty::Real | Ty::Any) {
                        return Err(unsupported("Antijoin Real/Any SQL equality"));
                    }
                }
            }
            let eq = lk
                .iter()
                .zip(rk)
                .map(|(a, b)| format!("l.c{a}=r.c{b}"))
                .collect::<Vec<_>>();
            if bag {
                format!(
                    "SELECT {} FROM {np}{l} l ANTI JOIN {np}{r} r ON {}",
                    bag_select(width, "l."),
                    if eq.is_empty() {
                        "true".into()
                    } else {
                        eq.join(" AND ")
                    }
                )
            } else {
                format!(
                "SELECT {} FROM {np}{l} l WHERE NOT EXISTS (SELECT 1 FROM {np}{r} r WHERE r.w>0{})",
                select(width, "l."),
                if eq.is_empty() {
                    String::new()
                } else {
                    format!(" AND {}", eq.join(" AND "))
                }
            )
            }
        }
        Op::Reduce { input, key, aggs } => {
            let ts = &types[*input as usize];
            if ts.iter().any(|t| matches!(t, Ty::Real | Ty::Any)) {
                return Err(unsupported("Reduce Real/Any SQL cell conversion"));
            }
            let mut out = key
                .iter()
                .enumerate()
                .map(|(i, c)| format!("c{c} AS c{i}"))
                .collect::<Vec<_>>();
            for agg in aggs {
                let value = match agg {
                    Agg::Count => {
                        if bag {
                            "count(*)::BIGINT".into()
                        } else {
                            "sum(w)::BIGINT".into()
                        }
                    }
                    Agg::Sum(c) => {
                        if bag {
                            format!("sum(c{c})::BIGINT")
                        } else {
                            format!("sum(c{c}::HUGEINT*w)::BIGINT")
                        }
                    }
                    Agg::Min(c) | Agg::Max(c) => {
                        let fun = if matches!(agg, Agg::Min(_)) {
                            "arg_min"
                        } else {
                            "arg_max"
                        };
                        if bag && ts[*c as usize] == Ty::Int {
                            format!(
                                "{}(c{c})",
                                if matches!(agg, Agg::Min(_)) {
                                    "min"
                                } else {
                                    "max"
                                }
                            )
                        } else {
                            format!(
                                "{fun}(c{c},{}){}",
                                ordered(ts[*c as usize], &format!("s.c{c}"))?,
                                if bag { "" } else { " FILTER (WHERE w>0)" }
                            )
                        }
                    }
                };
                out.push(format!("{value} AS c{}", out.len()));
            }
            if !bag {
                out.push("1::BIGINT AS w".into());
            } else if out.is_empty() {
                out.push("1::BIGINT AS unit".into());
            }
            let group = if key.is_empty() {
                String::new()
            } else {
                format!(
                    "GROUP BY {}",
                    key.iter()
                        .map(|c| format!("c{c}"))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            format!(
                "SELECT {} FROM {np}{input} s {group}{}",
                out.join(","),
                if !bag || key.is_empty() {
                    " HAVING count(*)>0"
                } else {
                    ""
                }
            )
        }
        Op::TopK {
            input,
            key,
            order,
            limit,
        } => {
            let ts = &types[*input as usize];
            let window = window_spec(ts, key, order, true)?;
            if bag {
                return Ok(Some(format!(
                    "SELECT {} FROM {np}{input} s QUALIFY row_number() OVER ({window})<={limit}",
                    bag_select(width, "s.")
                )));
            }
            let cs = cols(width, "");
            format!("SELECT {}1::BIGINT AS w FROM (SELECT {}, row_number() OVER ({window}) AS pos FROM {np}{input} s, LATERAL range(greatest(s.w,0)) copies) ranked WHERE pos<={limit}", if cs.is_empty() { String::new() } else { format!("{},", cs.join(",")) }, select(width,"s."))
        }
        Op::Window {
            input,
            partition,
            order,
            func,
        } => {
            let ts = &types[*input as usize];
            let spec = window_spec(
                ts,
                partition,
                order,
                !matches!(func, WinFn::Rank | WinFn::DenseRank),
            )?;
            let call = match func {
                WinFn::RowNumber => "row_number()".into(),
                WinFn::Rank => "rank()".into(),
                WinFn::DenseRank => "dense_rank()".into(),
                WinFn::Lag(n) => format!("lag(c{},{n},0)", order.first().map_or(0, |o| o.col)),
                WinFn::Lead(n) => format!("lead(c{},{n},0)", order.first().map_or(0, |o| o.col)),
                WinFn::Sum(c) => format!("sum(c{c})"),
                WinFn::Count => "count(*)".into(),
            };
            let frame = if matches!(func, WinFn::Sum(_) | WinFn::Count) {
                if order.is_empty() {
                    " ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING"
                } else {
                    " ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW"
                }
            } else {
                ""
            };
            if bag {
                return Ok(Some(format!(
                    "SELECT {}({call} OVER ({spec}{frame}))::BIGINT AS c{} FROM {np}{input} s",
                    if ts.is_empty() {
                        String::new()
                    } else {
                        format!("{},", cols(ts.len(), "s.").join(","))
                    },
                    ts.len()
                )));
            }
            format!("SELECT {}({call} OVER ({spec}{frame}))::BIGINT AS c{}, 1::BIGINT AS w FROM {np}{input} s, LATERAL range(greatest(s.w,0)) copies", if ts.is_empty() {String::new()} else {format!("{},",cols(ts.len(),"s.").join(","))},ts.len())
        }
    };
    Ok(Some(
        if bag || matches!(op, Op::Get(_) | Op::Negate(_) | Op::Threshold(_)) {
            q
        } else {
            consolidate(&q, width)
        },
    ))
}
fn window_spec(
    types: &[Ty],
    keys: &[ColId],
    order: &[Order],
    ties: bool,
) -> Result<String, EngineError> {
    if types.iter().any(|t| matches!(t, Ty::Real | Ty::Any)) {
        return Err(EngineError::new(
            Stage::Install,
            None,
            ErrorKind::Unsupported("Window/TopK Real/Any SQL cell conversion"),
        ));
    }
    let partition = if keys.is_empty() {
        String::new()
    } else {
        format!(
            "PARTITION BY {} ",
            keys.iter()
                .map(|c| format!("c{c}"))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let mut terms = order
        .iter()
        .map(|o| {
            Ok(format!(
                "{} {}",
                ordered(types[o.col as usize], &format!("s.c{}", o.col))?,
                if o.desc { "DESC" } else { "ASC" }
            ))
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    if ties {
        terms.extend(
            types
                .iter()
                .enumerate()
                .map(|(i, t)| ordered(*t, &format!("s.c{i}")))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(format!(
        "{partition}{}",
        if terms.is_empty() {
            String::new()
        } else {
            format!("ORDER BY {}", terms.join(","))
        }
    ))
}
