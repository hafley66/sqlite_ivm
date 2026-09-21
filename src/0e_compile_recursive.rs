use crate::catalog::error;
use crate::compile_from::{
    column_pair, equalities, part_expressions_mention, part_mentions, table_mentions,
};
use crate::relational::{
    alias, collation, column_reference, expression, expression_affinity, has_aggregate, key_sql,
    name, Compiler, Field, Kind, Occurrence, Rule,
};
use rusqlite::Result;
use sqlite3_parser::ast::*;
impl Compiler<'_> {
    pub(crate) fn recursive(&mut self, cte: &CommonTableExpr<'_>) -> Result<usize> {
        if cte.select.order_by.is_some() || cte.select.limit.is_some() {
            return Err(error("recursive ordering and LIMIT unsupported"));
        }
        let compounds = cte
            .select
            .body
            .compounds
            .as_ref()
            .ok_or_else(|| error("recursive UNION required"))?;
        if compounds
            .iter()
            .any(|c| c.operator == CompoundOperator::UnionAll)
        {
            return Err(error("recursive UNION ALL unsupported"));
        }
        if compounds
            .iter()
            .any(|c| c.operator != CompoundOperator::Union)
        {
            return Err(error("recursive compound requires UNION distinct"));
        }
        let member = name(cte.tbl_name.0);
        let parts = std::iter::once(&cte.select.body.select)
            .chain(compounds.iter().map(|c| &c.select))
            .collect::<Vec<_>>();
        let (steps, anchors): (Vec<&OneSelect<'_>>, Vec<&OneSelect<'_>>) = parts
            .into_iter()
            .partition(|part| part_mentions(part, &member));
        let mut inputs = vec![];
        for anchor in &anchors {
            inputs.push(self.core(anchor, None, None)?);
        }
        let width = self.plan.nodes[inputs[0]].fields.len();
        if inputs
            .iter()
            .any(|id| self.plan.nodes[*id].fields.len() != width)
        {
            return Err(error("compound column count mismatch"));
        }
        let mut member_fields = self.plan.nodes[inputs[0]].fields.clone();
        if let Some(columns) = cte.columns {
            if columns.len() != width {
                return Err(error("CTE column count mismatch"));
            }
            for (f, c) in member_fields.iter_mut().zip(columns) {
                f.name = name(c.col_name.0);
            }
        }
        for (i, f) in member_fields.iter_mut().enumerate() {
            f.qualifier = member.clone();
            f.visible = true;
            f.unqualified = true;
            f.position = i;
            f.merged_star = false;
        }
        let key_parts = |head: &[String]| {
            head.iter()
                .zip(&member_fields)
                .map(|(h, f)| (h.clone(), f.collation.clone()))
                .collect::<Vec<_>>()
        };
        let mut rules = vec![];
        for side in 0..inputs.len() {
            let head = (0..width).map(|i| format!("c{i}")).collect::<Vec<_>>();
            rules.push(Rule {
                occurrences: vec![(Occurrence::Input(side), width)],
                key: key_sql(&key_parts(&head)),
                head,
                predicate: None,
                indexes: vec![],
            });
        }
        for step in steps {
            let OneSelect::Select {
                columns,
                from: Some(from),
                where_clause,
                group_by: None,
                having: None,
                window_clause: None,
                ..
            } = step
            else {
                return Err(error("unsupported recursive step"));
            };
            let mut items = vec![(
                from.select
                    .ok_or_else(|| error("recursive source missing"))?,
                None,
            )];
            for join in from.joins.as_ref().map(|j| j.as_slice()).unwrap_or(&[]) {
                let inner = match join.operator {
                    JoinOperator::Comma | JoinOperator::TypedJoin(None) => true,
                    JoinOperator::TypedJoin(Some(t)) => {
                        t == JoinType::INNER || t == JoinType::CROSS
                    }
                };
                if !inner {
                    return Err(error("recursive step requires inner join"));
                }
                let on = match &join.constraint {
                    None => None,
                    Some(JoinConstraint::On(e)) => Some(e),
                    Some(JoinConstraint::Using(_)) => {
                        return Err(error("unsupported recursive step"))
                    }
                };
                items.push((&join.table, on));
            }
            let mut occurrences = vec![];
            let mut fields = vec![];
            let mut conjuncts = vec![];
            for (item, on) in items {
                match item {
                    SelectTable::Table(n, a, _)
                        if n.db_name.is_none() && name(n.name.0).eq_ignore_ascii_case(&member) =>
                    {
                        if occurrences.iter().any(|(o, _)| *o == Occurrence::Member) {
                            return Err(error("recursive step requires one recursive reference"));
                        }
                        let qualifier = alias(a).unwrap_or_else(|| member.clone());
                        occurrences.push((Occurrence::Member, width));
                        fields.extend(member_fields.iter().cloned().map(|mut f| {
                            f.qualifier = qualifier.clone();
                            f
                        }));
                    }
                    _ => {
                        if table_mentions(item, &member) {
                            return Err(error("unsupported recursive step"));
                        }
                        let id = self.table(item)?;
                        inputs.push(id);
                        occurrences.push((
                            Occurrence::Input(inputs.len() - 1),
                            self.plan.nodes[id].fields.len(),
                        ));
                        fields.extend(self.plan.nodes[id].fields.clone());
                    }
                }
                if let Some(on) = on {
                    conjuncts.push(on);
                }
            }
            if !occurrences.iter().any(|(o, _)| *o == Occurrence::Member) {
                return Err(error("recursive step requires one recursive reference"));
            }
            if let Some(e) = where_clause {
                conjuncts.push(e);
            }
            let predicate = conjuncts
                .iter()
                .map(|e| expression(e, &fields, false).map(|s| format!("({s})")))
                .collect::<Result<Vec<_>>>()?;
            let predicate = if predicate.is_empty() {
                None
            } else {
                Some(predicate.join(" AND "))
            };
            let mut head = vec![];
            for column in columns.iter() {
                match column {
                    ResultColumn::Star | ResultColumn::TableStar(_) => {
                        for (i, f) in fields.iter().enumerate().filter(|(_, f)| match column {
                            ResultColumn::TableStar(q) => {
                                f.qualifier.eq_ignore_ascii_case(&name(q.0))
                            }
                            _ => f.visible,
                        }) {
                            head.push((format!("c{i}"), f.affinity.clone(), f.collation.clone()));
                        }
                    }
                    ResultColumn::Expr(e, _) => {
                        if has_aggregate(e) {
                            return Err(error(
                                "recursive step may not aggregate or negate its own relation",
                            ));
                        }
                        head.push((
                            expression(e, &fields, false)?,
                            expression_affinity(e, &fields),
                            collation(e, &fields),
                        ));
                    }
                }
            }
            if head.len() != width {
                return Err(error("compound column count mismatch"));
            }
            for ((_, affinity, coll), f) in head.iter().zip(&member_fields) {
                if !affinity.is_empty() && !f.affinity.is_empty() && *affinity != f.affinity {
                    return Err(error("recursive key affinities must match"));
                }
                if *coll != f.collation {
                    return Err(error("recursive key collations must match"));
                }
            }
            let head = head.into_iter().map(|(h, _, _)| h).collect::<Vec<_>>();
            let mut indexes = vec![];
            let mut equal = vec![];
            for e in &conjuncts {
                equalities(e, &fields, &mut equal);
            }
            for (field, coll) in equal {
                let (mut local, mut occurrence) = (field, 0);
                while local >= occurrences[occurrence].1 {
                    local -= occurrences[occurrence].1;
                    occurrence += 1;
                }
                let entry = (
                    occurrences[occurrence].0.clone(),
                    format!(
                        "{} COLLATE {coll}",
                        column_reference(local, &fields[field].affinity)
                    ),
                );
                if !indexes.contains(&entry) {
                    indexes.push(entry);
                }
            }
            rules.push(Rule {
                occurrences,
                key: key_sql(&key_parts(&head)),
                head,
                predicate,
                indexes,
            });
        }
        Ok(self.push(Kind::Fixpoint { rules }, inputs, member_fields))
    }
}

/// Positions of conjuncts that are an equality between one column of the
/// already-joined left fields and one column of the incoming right table.
/// Those conjuncts become the step's ON; the rest stay in WHERE.
pub(crate) fn where_keys_for_step(keys: &[&Expr<'_>], fields: &[Field], split: usize) -> Vec<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, e)| column_pair(e, fields, split).is_some())
        .map(|(i, _)| i)
        .collect()
}

/// Named errors for recursion shapes SQLite itself rejects during prepare, so
/// the reason surfaces before SQLite's own message.
pub(crate) fn recursion_shape(s: &Select<'_>) -> Result<()> {
    if let Some(with) = &s.with {
        for cte in with.ctes {
            recursion_shape(cte.select)?;
            if !with.recursive {
                continue;
            }
            let member = name(cte.tbl_name.0);
            for part in cte.select.body.compounds.iter().flatten() {
                let step = part_mentions(&part.select, &member)
                    || part_expressions_mention(&part.select, &member);
                if !step {
                    continue;
                }
                if part.operator == CompoundOperator::UnionAll {
                    return Err(error("recursive UNION ALL unsupported"));
                }
                if let OneSelect::Select {
                    columns,
                    group_by,
                    having,
                    ..
                } = &part.select
                {
                    let aggregates = group_by.is_some()
                        || having.is_some()
                        || columns
                            .iter()
                            .any(|c| matches!(c, ResultColumn::Expr(e, _) if has_aggregate(e)));
                    if aggregates || part_expressions_mention(&part.select, &member) {
                        return Err(error(
                            "recursive step may not aggregate or negate its own relation",
                        ));
                    }
                }
            }
        }
    }
    for part in
        std::iter::once(&s.body.select).chain(s.body.compounds.iter().flatten().map(|c| &c.select))
    {
        if let OneSelect::Select {
            from: Some(from), ..
        } = part
        {
            for t in from
                .select
                .into_iter()
                .chain(from.joins.iter().flatten().map(|j| &j.table))
            {
                if let SelectTable::Select(inner, _) = t {
                    recursion_shape(inner)?;
                }
            }
        }
    }
    Ok(())
}
