use crate::catalog::error;
use crate::columns::{column_references, renumber_columns};
use crate::compile_from::{conjuncts, part_mentions};
use crate::relational::{
    alias, collation, direction, expression, expression_aliases, expression_affinity, field,
    has_aggregate, integer_limit, key_expression, name, nulls, ordinal, resolve, sql, Compiler, Field, Kind, Node,
};
use rusqlite::Result;
use sqlite3_parser::ast::*;
impl Compiler<'_> {
    pub(crate) fn push(&mut self, kind: Kind, inputs: Vec<usize>, fields: Vec<Field>) -> usize {
        let (kind, inputs) = self.project_group_input(kind, inputs);
        let id = self.plan.nodes.len();
        self.plan.nodes.push(Node {
            kind,
            inputs,
            fields,
        });
        id
    }
    /// A group arrangement stores every column of its input row, so a group
    /// over a join would keep the whole join product. A Map in front keeps
    /// only the columns the group reads, renumbered from c0.
    fn project_group_input(&mut self, kind: Kind, inputs: Vec<usize>) -> (Kind, Vec<usize>) {
        let Kind::Group {
            keys,
            expressions,
            order,
            limit,
            offset,
            having,
            window,
        } = kind
        else {
            return (kind, inputs);
        };
        let input = inputs[0];
        let width = self.plan.nodes[input].fields.len();
        let mut used = std::collections::BTreeSet::new();
        for sql in keys.iter().chain(&expressions).chain(&order).chain(&having) {
            used.extend(column_references(sql));
        }
        let kind = |keys, expressions, order, having| Kind::Group {
            keys,
            expressions,
            order,
            limit,
            offset,
            having,
            window,
        };
        if window || used.len() >= width || used.iter().any(|c| *c >= width) {
            return (kind(keys, expressions, order, having), inputs);
        }
        let mut kept = used.into_iter().collect::<Vec<_>>();
        if kept.is_empty() {
            // count(*) over no key still needs one stored column per row.
            kept.push(0);
        }
        let renumber = |sql: &String| {
            renumber_columns(sql, |c| kept.iter().position(|k| *k == c).unwrap_or(c))
        };
        let projected = kind(
            keys.iter().map(renumber).collect(),
            expressions.iter().map(renumber).collect(),
            order.iter().map(renumber).collect(),
            having.as_ref().map(renumber),
        );
        let fields = kept
            .iter()
            .map(|c| self.plan.nodes[input].fields[*c].clone())
            .collect();
        let map = self.push(
            Kind::Map {
                expressions: kept.iter().map(|c| format!("c{c}")).collect(),
                predicate: None,
            },
            vec![input],
            fields,
        );
        (projected, vec![map])
    }
    fn predicate(&mut self, id: usize, e: &Expr<'_>) -> Result<usize> {
        if let Expr::Binary(a, Operator::And, b) = e {
            let a = self.predicate(id, a)?;
            return self.predicate(a, b);
        }
        let exists = match e {
            Expr::Exists(s) => Some((*s, "semi")),
            Expr::Unary(UnaryOperator::Not, Expr::Exists(s)) => Some((*s, "anti")),
            _ => None,
        };
        if let Some((s, mode)) = exists {
            let OneSelect::Select {
                from: Some(f),
                where_clause: Some(on),
                group_by: None,
                having: None,
                window_clause: None,
                ..
            } = &s.body.select
            else {
                return Err(error("EXISTS requires a correlated equality"));
            };
            if s.limit.is_some() || s.body.compounds.is_some() || s.with.is_some() {
                return Err(error("EXISTS with LIMIT, compound or WITH unsupported"));
            }
            let right = self.from(f, &mut vec![])?;
            // An aggregate SELECT returns a row even for empty input. The
            // existence operator here requires the non-aggregate row shape.
            if let OneSelect::Select { columns, .. } = &s.body.select {
                let mut fields = self.plan.nodes[id].fields.clone();
                fields.extend(self.plan.nodes[right].fields.clone());
                for c in *columns {
                    if let ResultColumn::Expr(e, _) = c {
                        if has_aggregate(e) {
                            return Err(error(
                                "aggregate EXISTS requires scalar-subquery semantics",
                            ));
                        }
                        expression(e, &fields, false)?;
                    }
                }
            }
            return self.joined(id, right, &[on], mode);
        }
        let fields = self.plan.nodes[id].fields.clone();
        let predicate = Some(expression(e, &fields, false)?);
        let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
        Ok(self.push(
            Kind::Map {
                expressions,
                predicate,
            },
            vec![id],
            fields,
        ))
    }
    pub(crate) fn core(
        &mut self,
        s: &OneSelect<'_>,
        order: Option<&[SortedColumn<'_>]>,
        limit: Option<&Limit<'_>>,
    ) -> Result<usize> {
        let OneSelect::Select {
            distinctness,
            columns,
            from: Some(from),
            where_clause,
            group_by,
            having,
            window_clause,
        } = s
        else {
            return Err(error("unsupported SELECT core"));
        };
        let mut where_keys = vec![];
        if let Some(e) = where_clause {
            conjuncts(e, &mut where_keys);
        }
        let mut id = self.from(from, &mut where_keys)?;
        for e in where_keys {
            id = self.predicate(id, e)?;
        }
        let mut fields = self.plan.nodes[id].fields.clone();
        let aggregate = group_by.is_some()
            || columns
                .iter()
                .any(|c| matches!(c,ResultColumn::Expr(e,_) if has_aggregate(e)))
            || having.as_ref().is_some_and(|e| has_aggregate(e));
        let mut expressions = vec![];
        let mut output_fields = vec![];
        for column in *columns {
            match column {
                ResultColumn::Star | ResultColumn::TableStar(_) => {
                    let mut selected = fields
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| match column {
                            ResultColumn::TableStar(q) => {
                                f.qualifier.eq_ignore_ascii_case(&name(q.0))
                            }
                            _ => f.visible,
                        })
                        .collect::<Vec<_>>();
                    if matches!(column, ResultColumn::TableStar(_)) {
                        selected.sort_by_key(|(_, f)| f.position);
                    }
                    for (i, f) in selected {
                        let i = if matches!(column, ResultColumn::TableStar(_)) && f.merged_star {
                            fields
                                .iter()
                                .position(|m| {
                                    m.visible
                                        && m.unqualified
                                        && m.name.eq_ignore_ascii_case(&f.name)
                                })
                                .unwrap_or(i)
                        } else {
                            i
                        };
                        expressions.push(format!("c{i}"));
                        let mut f = f.clone();
                        f.visible = true;
                        f.unqualified = true;
                        output_fields.push(f);
                    }
                }
                ResultColumn::Expr(e, a) => {
                    let tail = match e {
                        Expr::FunctionCall {
                            filter_over: Some(t),
                            ..
                        }
                        | Expr::FunctionCallStar {
                            filter_over: Some(t),
                            ..
                        } => Some(t),
                        _ => None,
                    };
                    let value = if let Some(over) = tail.and_then(|t| t.over_clause) {
                        if aggregate {
                            return Err(error(
                                "aggregate/window composition requires a FROM subquery",
                            ));
                        }
                        let w = match over {
                            Over::Window(w) => *w,
                            Over::Name(n) => {
                                &window_clause
                                    .unwrap_or(&[])
                                    .iter()
                                    .find(|w| w.name.0.eq_ignore_ascii_case(n.0))
                                    .ok_or_else(|| error("unknown window"))?
                                    .window
                            }
                        };
                        if w.base.is_some() {
                            return Err(error("inherited windows unsupported"));
                        }
                        let keys = w
                            .partition_by
                            .unwrap_or(&[])
                            .iter()
                            .map(|e| {
                                expression(e, &fields, false)
                                    .map(|s| key_expression(&s, &collation(e, &fields)))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let ordering = w
                            .order_by
                            .unwrap_or(&[])
                            .iter()
                            .map(|s| {
                                Ok(format!(
                                    "{} {}{}",
                                    expression(&s.expr, &fields, false)?,
                                    direction(s),
                                    nulls(s)
                                ))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let (function, args) = match e {
                            Expr::FunctionCall {
                                name,
                                args,
                                distinctness: None,
                                order_by: None,
                                ..
                            } => (
                                name.0.to_ascii_lowercase(),
                                args.unwrap_or(&[])
                                    .iter()
                                    .map(|e| expression(e, &fields, false))
                                    .collect::<Result<Vec<_>>>()?
                                    .join(","),
                            ),
                            Expr::FunctionCallStar { name, .. } => {
                                (name.0.to_ascii_lowercase(), "*".into())
                            }
                            _ => return Err(error("unsupported window call")),
                        };
                        if ![
                            "row_number",
                            "rank",
                            "dense_rank",
                            "percent_rank",
                            "cume_dist",
                            "ntile",
                            "lag",
                            "lead",
                            "first_value",
                            "last_value",
                            "nth_value",
                            "count",
                            "sum",
                            "avg",
                            "min",
                            "max",
                        ]
                        .contains(&function.as_str())
                        {
                            return Err(error("unsupported window function"));
                        }
                        let filter = tail
                            .and_then(|t| t.filter_clause)
                            .map(|e| {
                                expression(e, &fields, false).map(|v| format!(" FILTER(WHERE {v})"))
                            })
                            .transpose()?
                            .unwrap_or_default();
                        let over = format!(
                            "{} {}",
                            if ordering.is_empty() {
                                String::new()
                            } else {
                                format!("ORDER BY {}", ordering.join(","))
                            },
                            w.frame_clause.as_ref().map(sql).unwrap_or_default()
                        );
                        let mut values = (0..fields.len())
                            .map(|i| format!("c{i}"))
                            .collect::<Vec<_>>();
                        values.push(format!("{function}({args}){filter} OVER({over})"));
                        let value = format!("c{}", fields.len());
                        fields.push(field(
                            String::new(),
                            format!("__ivm_window{}", fields.len()),
                            String::new(),
                            "BINARY".into(),
                            false,
                            false,
                        ));
                        id = self.push(
                            Kind::Group {
                                keys,
                                expressions: values,
                                order: vec![],
                                limit: None,
                                offset: 0,
                                having: None,
                                window: true,
                            },
                            vec![id],
                            fields.clone(),
                        );
                        value
                    } else {
                        expression(e, &fields, aggregate)?
                    };
                    expressions.push(value);
                    output_fields.push(field(
                        String::new(),
                        alias(a).unwrap_or_else(|| match e {
                            Expr::Name(n) | Expr::Qualified(_, n) => name(n.0),
                            Expr::Id(n) => name(n.0),
                            _ => e.to_string(),
                        }),
                        expression_affinity(e, &fields),
                        collation(e, &fields),
                        true,
                        true,
                    ));
                }
            }
        }
        let output_width = expressions.len();
        let aliases = output_fields
            .iter()
            .zip(&expressions)
            .map(|(f, e)| (f.name.clone(), e.clone()))
            .collect::<Vec<_>>();
        let keys = group_by
            .unwrap_or(&[])
            .iter()
            .map(|e| {
                if let Some(i) = ordinal(e, expressions.len())? {
                    Ok(key_expression(&expressions[i], &output_fields[i].collation))
                } else {
                    expression_aliases(e, &fields, false, &aliases).map(|v| {
                        let c = if resolve(e, &fields).is_ok() {
                            collation(e, &fields)
                        } else {
                            resolve(e, &output_fields)
                                .map(|i| output_fields[i].collation.clone())
                                .unwrap_or_else(|_| collation(e, &fields))
                        };
                        key_expression(&v, &c)
                    })
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let having = having
            .as_ref()
            .map(|e| expression_aliases(e, &fields, true, &aliases))
            .transpose()?;
        let mut ordering = vec![];
        if limit.is_some() {
            for s in order.unwrap_or(&[]) {
                let existing = ordinal(&s.expr, output_width)?
                    .or_else(|| resolve(&s.expr, &output_fields).ok())
                    .or_else(|| {
                        columns
                            .iter()
                            .position(|c| matches!(c,ResultColumn::Expr(e,_) if *e==s.expr))
                    });
                let index = if let Some(i) = existing {
                    i
                } else {
                    if *distinctness == Some(Distinctness::Distinct) {
                        return Err(error("DISTINCT top-k ordering must refer to an output"));
                    }
                    let value = expression_aliases(&s.expr, &fields, aggregate, &aliases)?;
                    let i = expressions.len();
                    expressions.push(value);
                    output_fields.push(field(
                        String::new(),
                        format!("__ivm_order{i}"),
                        String::new(),
                        "BINARY".into(),
                        false,
                        false,
                    ));
                    i
                };
                ordering.push(format!("c{index} {}{}", direction(s), nulls(s)));
            }
        }
        id = if aggregate {
            self.push(
                Kind::Group {
                    keys,
                    expressions,
                    order: vec![],
                    limit: None,
                    offset: 0,
                    having,
                    window: false,
                },
                vec![id],
                output_fields,
            )
        } else {
            self.push(
                Kind::Map {
                    expressions,
                    predicate: None,
                },
                vec![id],
                output_fields,
            )
        };
        if *distinctness == Some(Distinctness::Distinct) {
            let fs = self.plan.nodes[id].fields.clone();
            id = self.push(Kind::Set("distinct"), vec![id], fs);
        }
        if let Some(l) = limit {
            let count = integer_limit(&l.expr)?;
            let offset = l
                .offset
                .as_ref()
                .map(integer_limit)
                .transpose()?
                .unwrap_or(0)
                .max(0);
            let fs = self.plan.nodes[id].fields.clone();
            let expressions = (0..output_width).map(|i| format!("c{i}")).collect();
            id = self.push(
                Kind::Group {
                    keys: vec![],
                    expressions,
                    order: ordering,
                    limit: Some(count),
                    offset,
                    having: None,
                    window: false,
                },
                vec![id],
                fs[..output_width].to_vec(),
            );
        }
        Ok(id)
    }
    pub(crate) fn select(&mut self, s: &Select<'_>) -> Result<usize> {
        let mark = self.ctes.len();
        if let Some(with) = &s.with {
            for cte in with.ctes {
                let member = name(cte.tbl_name.0);
                let recursive = with.recursive
                    && cte.select.body.compounds.as_ref().is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|part| part_mentions(&part.select, &member))
                    });
                let mut id = if recursive {
                    self.recursive(cte)?
                } else {
                    self.select(cte.select)?
                };
                if let Some(columns) = cte.columns {
                    let mut fields = self.plan.nodes[id].fields.clone();
                    if columns.len() != fields.len() {
                        return Err(error("CTE column count mismatch"));
                    }
                    for (f, c) in fields.iter_mut().zip(columns) {
                        f.name = name(c.col_name.0);
                    }
                    let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
                    id = self.push(
                        Kind::Map {
                            expressions,
                            predicate: None,
                        },
                        vec![id],
                        fields,
                    );
                }
                self.ctes.push((name(cte.tbl_name.0), id));
            }
        }
        let mut id = self.core(
            &s.body.select,
            s.order_by,
            if s.body.compounds.is_some() {
                None
            } else {
                s.limit
            },
        )?;
        if let Some(compounds) = &s.body.compounds {
            for c in compounds {
                let right = self.core(&c.select, None, None)?;
                let fields = self.plan.nodes[id].fields.clone();
                if fields.len() != self.plan.nodes[right].fields.len() {
                    return Err(error("compound column count mismatch"));
                }
                let op = match c.operator {
                    CompoundOperator::Union => "union",
                    CompoundOperator::UnionAll => "all",
                    CompoundOperator::Except => "except",
                    CompoundOperator::Intersect => "intersect",
                };
                id = self.push(Kind::Set(op), vec![id, right], fields);
            }
        }
        if s.body.compounds.is_some() {
            if let Some(l) = s.limit {
                let fields = self.plan.nodes[id].fields.clone();
                let order = s
                    .order_by
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| {
                        let value = if let Some(i) = ordinal(&s.expr, fields.len())? {
                            format!("c{i}")
                        } else {
                            expression(&s.expr, &fields, false)?
                        };
                        Ok(format!("{value} {}{}", direction(s), nulls(s)))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let count = integer_limit(&l.expr)?;
                let offset = l
                    .offset
                    .as_ref()
                    .map(integer_limit)
                    .transpose()?
                    .unwrap_or(0)
                    .max(0);
                let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
                id = self.push(
                    Kind::Group {
                        keys: vec![],
                        expressions,
                        order,
                        limit: Some(count),
                        offset,
                        having: None,
                        window: false,
                    },
                    vec![id],
                    fields,
                );
            }
        }
        self.ctes.truncate(mark);
        Ok(id)
    }
}
