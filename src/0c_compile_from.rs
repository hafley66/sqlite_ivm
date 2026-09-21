use crate::catalog::error;
use crate::compile_recursive::where_keys_for_step;
use crate::relational::{
    affinity, alias, explicit_collation, expression, field, implicit_collation, name, resolve,
    Compiler, Field, Kind, Source,
};
use rusqlite::Result;
use sqlite3_parser::ast::*;
impl Compiler<'_> {
    pub(crate) fn table(&mut self, t: &SelectTable<'_>) -> Result<usize> {
        if let SelectTable::Sub(from, None) = t {
            return self.from(from, &mut vec![]);
        }
        let (id, q) = match t {
            SelectTable::Select(s, a) => (self.select(s)?, alias(a).unwrap_or_default()),
            SelectTable::Sub(from, a) => {
                (self.from(from, &mut vec![])?, alias(a).unwrap_or_default())
            }
            SelectTable::Table(t, a, _) => {
                if t.db_name
                    .as_ref()
                    .is_some_and(|n| !name(n.0).eq_ignore_ascii_case("main"))
                {
                    return Err(error("sources must be in main"));
                }
                let table = name(t.name.0);
                let qualifier = alias(a).unwrap_or_else(|| table.clone());
                if let Some((_, id)) = self
                    .ctes
                    .iter()
                    .rev()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&table))
                {
                    (*id, qualifier)
                } else {
                    let actual:String=self.db.query_row("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name=?1 COLLATE NOCASE",[&table],|r|r.get(0))?;
                    if actual.starts_with("__ivm_") || actual.starts_with("sqlite_") {
                        return Err(error("internal source table"));
                    }
                    if self.db.query_row("SELECT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name=?1 COLLATE NOCASE)",[&actual],|r|r.get::<_,bool>(0))?{return Err(error("temporary source shadow"));}
                    let source = if let Some(i) =
                        self.plan.sources.iter().position(|s| s.name == actual)
                    {
                        i
                    } else {
                        let columns=self.db.prepare("SELECT name FROM pragma_table_xinfo(?1,'main') WHERE hidden<>1 ORDER BY cid")?.query_map([&actual],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?;
                        let affinities=self.db.prepare("SELECT type FROM pragma_table_xinfo(?1,'main') WHERE hidden<>1 ORDER BY cid")?.query_map([&actual],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?.iter().map(|s|affinity(s)).collect();
                        let mut collations = vec![];
                        for column in &columns {
                            let table_c = std::ffi::CString::new(actual.as_str())
                                .map_err(|e| error(e.to_string()))?;
                            let column_c = std::ffi::CString::new(column.as_str())
                                .map_err(|e| error(e.to_string()))?;
                            let mut collation = std::ptr::null();
                            let rc = unsafe {
                                rusqlite::ffi::sqlite3_table_column_metadata(
                                    self.db.handle(),
                                    c"main".as_ptr(),
                                    table_c.as_ptr(),
                                    column_c.as_ptr(),
                                    std::ptr::null_mut(),
                                    &mut collation,
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                )
                            };
                            if rc != rusqlite::ffi::SQLITE_OK || collation.is_null() {
                                return Err(error("source collation metadata unavailable"));
                            }
                            let collation = unsafe { std::ffi::CStr::from_ptr(collation) }
                                .to_string_lossy()
                                .to_ascii_uppercase();
                            if !["BINARY", "NOCASE", "RTRIM"].contains(&collation.as_str()) {
                                return Err(error("unsupported source collation"));
                            }
                            collations.push(collation);
                        }
                        self.plan.sources.push(Source {
                            name: actual,
                            columns,
                            affinities,
                            collations,
                        });
                        self.plan.sources.len() - 1
                    };
                    let fields = self.plan.sources[source]
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            field(
                                qualifier.clone(),
                                n.clone(),
                                self.plan.sources[source].affinities[i].clone(),
                                self.plan.sources[source].collations[i].clone(),
                                true,
                                true,
                            )
                        })
                        .collect();
                    (self.push(Kind::Input(source), vec![], fields), qualifier)
                }
            }
            _ => return Err(error("unsupported FROM source")),
        };
        let positions = self.plan.nodes[id]
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.visible)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let mut fields = positions
            .iter()
            .map(|i| self.plan.nodes[id].fields[*i].clone())
            .collect::<Vec<_>>();
        for (i, f) in fields.iter_mut().enumerate() {
            f.qualifier = q.clone();
            f.unqualified = true;
            f.position = i;
            f.merged_star = false;
        }
        // Alias nodes share upstream state, while preserving independent column scopes.
        let expressions = positions.iter().map(|i| format!("c{i}")).collect();
        Ok(self.push(
            Kind::Map {
                expressions,
                predicate: None,
            },
            vec![id],
            fields,
        ))
    }
    pub(crate) fn joined(
        &mut self,
        left: usize,
        right: usize,
        ons: &[&Expr<'_>],
        mode: &'static str,
    ) -> Result<usize> {
        let split = self.plan.nodes[left].fields.len();
        let mut fields = self.plan.nodes[left].fields.clone();
        fields.extend(self.plan.nodes[right].fields.clone());
        let (mut l, mut r) = (vec![], vec![]);
        for on in ons {
            index_pairs(on, &fields, split, &mut l, &mut r);
        }
        let (mut strict_l, mut strict_r) = (vec![], vec![]);
        let pure = ons
            .iter()
            .all(|on| pairs(on, &fields, split, &mut strict_l, &mut strict_r).is_ok())
            && strict_l == l
            && strict_r == r;
        let predicate = if pure {
            None
        } else {
            Some(
                ons.iter()
                    .map(|on| {
                        expression(on, &fields, false).map(|s| {
                            if ons.len() > 1 {
                                format!("({s})")
                            } else {
                                s
                            }
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(" AND "),
            )
        };
        if mode == "semi" || mode == "anti" {
            fields.truncate(split);
        }
        Ok(self.push(
            Kind::Join {
                left: l,
                right: r,
                mode,
                predicate,
            },
            vec![left, right],
            fields,
        ))
    }
    pub(crate) fn from<'a>(
        &mut self,
        from: &FromClause<'a>,
        where_keys: &mut Vec<&'a Expr<'a>>,
    ) -> Result<usize> {
        let mut id = self.table(from.select.ok_or_else(|| error("source required"))?)?;
        if let Some(joins) = &from.joins {
            for join in joins {
                let right = self.table(&join.table)?;
                let typ = match join.operator {
                    JoinOperator::TypedJoin(Some(t)) => t,
                    _ => JoinType::INNER,
                };
                let mode = match (typ.contains(JoinType::LEFT), typ.contains(JoinType::RIGHT)) {
                    (true, true) => "full",
                    (true, false) => "left",
                    (false, true) => "right",
                    _ => "inner",
                };
                if join.constraint.is_none()
                    && mode == "inner"
                    && !typ.contains(JoinType::NATURAL)
                    && matches!(
                        join.operator,
                        JoinOperator::Comma | JoinOperator::TypedJoin(None)
                    )
                {
                    let split = self.plan.nodes[id].fields.len();
                    let mut fields = self.plan.nodes[id].fields.clone();
                    fields.extend(self.plan.nodes[right].fields.clone());
                    let taken = where_keys_for_step(where_keys, &fields, split);
                    if !taken.is_empty() {
                        let taken_ons = taken
                            .iter()
                            .map(|&i| where_keys[i] as &'a Expr<'a>)
                            .collect::<Vec<_>>();
                        for i in taken.into_iter().rev() {
                            where_keys.remove(i);
                        }
                        id = self.joined(id, right, &taken_ons, mode)?;
                        continue;
                    }
                }
                if let Some(JoinConstraint::On(on)) = &join.constraint {
                    id = self.joined(id, right, &[on], mode)?;
                    continue;
                }
                let split = self.plan.nodes[id].fields.len();
                let mut fields = self.plan.nodes[id].fields.clone();
                fields.extend(self.plan.nodes[right].fields.clone());
                let using = match &join.constraint {
                    Some(JoinConstraint::Using(names)) => {
                        names.iter().map(|n| name(n.0)).collect::<Vec<_>>()
                    }
                    _ if typ.contains(JoinType::NATURAL) => fields[..split]
                        .iter()
                        .filter(|f| {
                            f.visible
                                && fields[split..]
                                    .iter()
                                    .any(|r| r.visible && f.name.eq_ignore_ascii_case(&r.name))
                        })
                        .map(|f| f.name.clone())
                        .collect(),
                    _ => vec![],
                };
                let (mut l, mut r) = (vec![], vec![]);
                for n in using {
                    let find = |fs: &[Field]| -> Result<usize> {
                        let ids = fs
                            .iter()
                            .enumerate()
                            .filter(|(_, f)| f.unqualified && f.name.eq_ignore_ascii_case(&n))
                            .map(|(i, _)| i)
                            .collect::<Vec<_>>();
                        if ids.len() == 1 {
                            Ok(ids[0])
                        } else {
                            Err(error("ambiguous USING column"))
                        }
                    };
                    l.push(find(&fields[..split])?);
                    r.push(find(&fields[split..])?);
                }
                let indexed = l.iter().zip(&r).all(|(a, b)| {
                    fields[*a].affinity == fields[split + b].affinity
                        && fields[*a].collation == fields[split + b].collation
                });
                let value = |i: usize| {
                    if fields[i].affinity.is_empty() {
                        format!("c{i}")
                    } else {
                        format!("CAST(c{i} AS {})", fields[i].affinity)
                    }
                };
                let predicate = if indexed {
                    None
                } else {
                    Some(
                        l.iter()
                            .zip(&r)
                            .map(|(a, b)| {
                                format!(
                                    "({} COLLATE {})={}",
                                    value(*a),
                                    fields[*a].collation,
                                    value(split + b)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" AND "),
                    )
                };
                id = self.push(
                    Kind::Join {
                        left: if indexed { l.clone() } else { vec![] },
                        right: if indexed { r.clone() } else { vec![] },
                        mode,
                        predicate,
                    },
                    vec![id, right],
                    fields.clone(),
                );
                let mut expressions = (0..fields.len())
                    .map(|i| format!("c{i}"))
                    .collect::<Vec<_>>();
                // Keep each original qualified key, and expose the SQL USING key
                // separately. FULL JOIN selects the non-NULL side of that key.
                for (a, b) in l.iter().zip(&r) {
                    let mut merged = fields[*a].clone();
                    merged.qualifier.clear();
                    merged.visible = true;
                    merged.unqualified = true;
                    fields[*a].merged_star = true;
                    fields[*a].visible = false;
                    fields[*a].unqualified = false;
                    fields[split + b].visible = false;
                    fields[split + b].unqualified = false;
                    expressions.push(match mode {
                        "full" => format!("coalesce(c{a},c{})", split + b),
                        "right" => format!("c{}", split + b),
                        _ => format!("c{a}"),
                    });
                    fields.push(merged);
                }
                if !l.is_empty() {
                    // SQL star keeps left-side column order, replacing its USING
                    // columns by the merged values; right-side keys are omitted.
                    let mut positions = (0..split)
                        .map(|i| {
                            l.iter()
                                .position(|a| *a == i)
                                .map_or(i, |p| split + self.plan.nodes[right].fields.len() + p)
                        })
                        .collect::<Vec<_>>();
                    positions.extend(
                        (split..split + self.plan.nodes[right].fields.len())
                            .filter(|i| !r.contains(&(i - split))),
                    );
                    positions.extend(
                        (0..fields.len())
                            .filter(|i| !positions.contains(i))
                            .collect::<Vec<_>>(),
                    );
                    let ordered_fields = positions.iter().map(|i| fields[*i].clone()).collect();
                    let ordered_exprs = positions.iter().map(|i| expressions[*i].clone()).collect();
                    id = self.push(
                        Kind::Map {
                            expressions: ordered_exprs,
                            predicate: None,
                        },
                        vec![id],
                        ordered_fields,
                    );
                }
            }
        }
        Ok(id)
    }
}

fn pairs(
    e: &Expr<'_>,
    fields: &[Field],
    split: usize,
    l: &mut Vec<usize>,
    r: &mut Vec<usize>,
) -> Result<()> {
    match e {
        Expr::Parenthesized(es) if es.len() == 1 => pairs(&es[0], fields, split, l, r),
        Expr::Binary(a, Operator::And, b) => {
            pairs(a, fields, split, l, r)?;
            pairs(b, fields, split, l, r)
        }
        Expr::Binary(a, Operator::Equals, b) => {
            let (a, b) = (resolve(a, fields)?, resolve(b, fields)?);
            if a < split && b >= split {
                l.push(a);
                r.push(b - split);
            } else if b < split && a >= split {
                l.push(b);
                r.push(a - split);
            } else {
                return Err(error("join equality must connect its inputs"));
            }
            Ok(())
        }
        _ => Err(error("join requires column equality conjunctions")),
    }
}
fn index_pairs(
    e: &Expr<'_>,
    fields: &[Field],
    split: usize,
    l: &mut Vec<usize>,
    r: &mut Vec<usize>,
) {
    match e {
        Expr::Parenthesized(es) if es.len() == 1 => index_pairs(&es[0], fields, split, l, r),
        Expr::Binary(a, Operator::And, b) => {
            index_pairs(a, fields, split, l, r);
            index_pairs(b, fields, split, l, r);
        }
        Expr::Binary(a, Operator::Equals, b) => {
            if let (Ok(a), Ok(b)) = (resolve(a, fields), resolve(b, fields)) {
                if fields[a].affinity == fields[b].affinity
                    && fields[a].collation == fields[b].collation
                {
                    if a < split && b >= split {
                        l.push(a);
                        r.push(b - split);
                    } else if b < split && a >= split {
                        l.push(b);
                        r.push(a - split);
                    }
                }
            }
        }
        _ => {}
    }
}
pub(crate) fn table_mentions(t: &SelectTable<'_>, member: &str) -> bool {
    match t {
        SelectTable::Table(n, _, _) => {
            n.db_name.is_none() && name(n.name.0).eq_ignore_ascii_case(member)
        }
        SelectTable::Select(s, _) => select_mentions(s, member),
        SelectTable::Sub(from, _) => from_mentions(from, member),
        SelectTable::TableCall(..) => false,
    }
}
/// Flattens an AND chain into its equality and filter leaves.
pub(crate) fn conjuncts<'a>(e: &'a Expr<'a>, out: &mut Vec<&'a Expr<'a>>) {
    if let Expr::Binary(a, Operator::And, b) = e {
        conjuncts(a, out);
        conjuncts(b, out);
    } else {
        out.push(e);
    }
}
pub(crate) fn column_pair<'a>(e: &'a Expr<'a>, fields: &[Field], split: usize) -> Option<(usize, usize)> {
    let inner = |e: &'a Expr<'a>| match e {
        Expr::Parenthesized(es) if es.len() == 1 => &es[0],
        _ => e,
    };
    let Expr::Binary(a, Operator::Equals, b) = inner(e) else {
        return None;
    };
    let (x, y) = (resolve(a, fields).ok()?, resolve(b, fields).ok()?);
    if x < split && y >= split {
        Some((x, y - split))
    } else if y < split && x >= split {
        Some((y, x - split))
    } else {
        None
    }
}
fn from_mentions(from: &FromClause<'_>, member: &str) -> bool {
    from.select.is_some_and(|t| table_mentions(t, member))
        || from
            .joins
            .as_ref()
            .is_some_and(|joins| joins.iter().any(|j| table_mentions(&j.table, member)))
}
pub(crate) fn part_mentions(part: &OneSelect<'_>, member: &str) -> bool {
    match part {
        OneSelect::Select {
            from: Some(from), ..
        } => from_mentions(from, member),
        _ => false,
    }
}
fn select_mentions(s: &Select<'_>, member: &str) -> bool {
    s.with
        .as_ref()
        .is_some_and(|w| w.ctes.iter().any(|c| select_mentions(c.select, member)))
        || std::iter::once(&s.body.select)
            .chain(s.body.compounds.iter().flatten().map(|c| &c.select))
            .any(|part| part_mentions(part, member) || part_expressions_mention(part, member))
}
pub(crate) fn part_expressions_mention(part: &OneSelect<'_>, member: &str) -> bool {
    let OneSelect::Select {
        columns,
        where_clause,
        having,
        ..
    } = part
    else {
        return false;
    };
    columns
        .iter()
        .any(|c| matches!(c, ResultColumn::Expr(e, _) if expr_mentions(e, member)))
        || where_clause.is_some_and(|e| expr_mentions(e, member))
        || having.is_some_and(|e| expr_mentions(e, member))
}
fn expr_mentions(e: &Expr<'_>, member: &str) -> bool {
    let sub = |e: &Expr<'_>| expr_mentions(e, member);
    match e {
        Expr::Exists(s) | Expr::Subquery(s) => select_mentions(s, member),
        Expr::InSelect { lhs, rhs, .. } => sub(lhs) || select_mentions(rhs, member),
        Expr::Binary(a, _, b) => sub(a) || sub(b),
        Expr::Unary(_, e)
        | Expr::IsNull(e)
        | Expr::NotNull(e)
        | Expr::Cast { expr: e, .. }
        | Expr::Collate(e, _) => sub(e),
        Expr::Parenthesized(es) => es.iter().any(sub),
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            base.is_some_and(sub)
                || else_expr.is_some_and(sub)
                || when_then_pairs.iter().any(|(a, b)| sub(a) || sub(b))
        }
        Expr::Between {
            lhs, start, end, ..
        } => sub(lhs) || sub(start) || sub(end),
        Expr::Like {
            lhs, rhs, escape, ..
        } => sub(lhs) || sub(rhs) || escape.is_some_and(sub),
        Expr::InList { lhs, rhs, .. } => sub(lhs) || rhs.unwrap_or(&[]).iter().any(sub),
        Expr::FunctionCall { args, .. } => args.unwrap_or(&[]).iter().any(sub),
        _ => false,
    }
}
pub(crate) fn equalities(e: &Expr<'_>, fields: &[Field], out: &mut Vec<(usize, String)>) {
    match e {
        Expr::Parenthesized(es) if es.len() == 1 => equalities(&es[0], fields, out),
        Expr::Binary(a, Operator::And, b) => {
            equalities(a, fields, out);
            equalities(b, fields, out);
        }
        Expr::Binary(a, Operator::Equals, b) => {
            if let (Ok(l), Ok(r)) = (resolve(a, fields), resolve(b, fields)) {
                let coll = explicit_collation(a)
                    .or_else(|| explicit_collation(b))
                    .or_else(|| implicit_collation(a, fields))
                    .or_else(|| implicit_collation(b, fields))
                    .unwrap_or("BINARY".into());
                out.push((l, coll.clone()));
                out.push((r, coll));
            }
        }
        _ => {}
    }
}
