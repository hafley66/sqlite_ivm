use crate::{
    query::{error, quote},
    relational::{Kind, Plan, Rule},
    relational_maintenance::{
        columns, keys_table, max_rowid, out_table, roles, rule_from, rule_where, table,
        BULK_GROUP_BUDGET, BULK_MULTIPLICITY_BUDGET, BULK_ROUND_BUDGET, CachedExecute, Role,
    },
};
use rusqlite::{params_from_iter, Connection, Result};

impl Plan {
    pub(crate) fn materialize(&self, db: &Connection, name: &str, id: usize, restricted: bool) -> Result<()> {
        let node = &self.nodes[id];
        match &node.kind {
            Kind::Input(_) => self.materialize_input(db, id)?,
            Kind::Map { .. } => self.materialize_map(db, id)?,
            Kind::Set(_) => self.materialize_set(db, name, id, restricted)?,
            Kind::Join { .. } => self.materialize_join(db, name, id, restricted)?,
            Kind::Group { .. } => self.materialize_group(db, name, id, restricted)?,
            Kind::Fixpoint { .. } => self.materialize_fixpoint(db, name, id)?,
        }
        Ok(())
    }
    fn materialize_input(&self, db: &Connection, id: usize) -> Result<()> {
        let out = out_table(id, self.nodes[id].fields.len());
        let cols = columns(self.nodes[id].fields.len());
        let Kind::Input(source) = &self.nodes[id].kind else {
            unreachable!()
        };
        let s = &self.sources[*source];
        db.execute_cached(
            &format!(
                "INSERT INTO {out}({cols},__m) SELECT {},1 FROM main.{}",
                s.columns
                    .iter()
                    .map(|c| quote(c))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&s.name)
            ),
            [],
        )?;
        Ok(())
    }
    fn materialize_map(&self, db: &Connection, id: usize) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        let Kind::Map { expressions, predicate } = &node.kind else {
            unreachable!()
        };
        let child = out_table(node.inputs[0], self.nodes[node.inputs[0]].fields.len());
        db.execute_cached(
            &format!(
                "INSERT INTO {out}({cols},__m) SELECT {},__m FROM {child}{}",
                expressions.join(","),
                predicate
                    .as_ref()
                    .map(|p| format!(" WHERE {p}"))
                    .unwrap_or_default()
            ),
            [],
        )?;
        Ok(())
    }
    fn materialize_set(&self, db: &Connection, name: &str, id: usize, restricted: bool) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        let Kind::Set(op) = &node.kind else {
            unreachable!()
        };
        let width = node.fields.len();
        let reps = |t: &str| {
            format!(
                "SELECT a.c0{},1 FROM {t} a WHERE a.rowid=(SELECT MIN(rowid) FROM {t} b WHERE b.__k=a.__k)",
                (1..width).map(|i| format!(",a.c{i}")).collect::<String>()
            )
        };
        let t0 = self.source_table(db, name, id, 0, restricted)?;
        let t1 = if node.inputs.len() == 2 {
            self.source_table(db, name, id, 1, restricted)?
        } else {
            String::new()
        };
        let sql = match (*op, node.inputs.len()) {
            ("all", _) => {
                let child =
                    out_table(node.inputs[0], self.nodes[node.inputs[0]].fields.len());
                format!("INSERT INTO {out}({cols},__m) SELECT {cols},__m FROM {child}")
            }
            ("distinct", _) | (_, 1) => {
                format!("INSERT INTO {out}({cols},__m) {}", reps(&t0))
            }
            ("union", _) => format!(
                "INSERT INTO {out}({cols},__m) {} UNION ALL SELECT a.c0{},1 FROM {} a WHERE a.rowid=(SELECT MIN(rowid) FROM {} b WHERE b.__k=a.__k) AND a.__k NOT IN(SELECT __k FROM {})",
                reps(&t0),
                (1..width).map(|i| format!(",a.c{i}")).collect::<String>(),
                t1.clone(),
                t1.clone(),
                t0
            ),
            ("except", _) => format!(
                "INSERT INTO {out}({cols},__m) {} AND a.__k NOT IN(SELECT __k FROM {})",
                reps(&t0),
                t1.clone()
            ),
            _ => format!(
                "INSERT INTO {out}({cols},__m) {} AND a.__k IN(SELECT __k FROM {})",
                reps(&t0),
                t1.clone()
            ),
        };
        db.execute_cached(&sql, [])?;
        Ok(())
    }
    fn materialize_join(&self, db: &Connection, name: &str, id: usize, restricted: bool) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        let Kind::Join { mode, predicate, .. } = &node.kind else {
            unreachable!()
        };
        let left_n = self.nodes[node.inputs[0]].fields.len();
        let right_n = self.nodes[node.inputs[1]].fields.len();
        let t0 = self.source_table(db, name, id, 0, restricted)?;
        let t1 = self.source_table(db, name, id, 1, restricted)?;
        // A join key equal on both sides still cannot match when a
        // component is NULL, and the components live in the dictionary.
        let dict = keys_table(name);
        let no_null = |k: &str| {
            format!("NOT EXISTS(SELECT 1 FROM json_each((SELECT __v FROM {dict} WHERE __i={k})) WHERE type='null')")
        };
        let left_cols = |q: &str| {
            (0..left_n)
                .map(|i| format!("{q}.c{i} AS c{i}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let right_cols = |q: &str| {
            (0..right_n)
                .map(|i| format!("{q}.c{i} AS c{}", left_n + i))
                .collect::<Vec<_>>()
                .join(",")
        };
        let null_cols = |from: usize, n: usize| {
            (from..from + n)
                .map(|i| format!("NULL AS c{i}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let left_pair = |outer: &str, inner: &str| {
            let mut parts: Vec<String> = (0..left_n)
                .map(|i| format!("{outer}.c{i} AS c{i}"))
                .collect();
            parts.extend((0..right_n).map(|j| format!("{inner}.c{j} AS c{}", left_n + j)));
            parts.join(",")
        };
        let right_pair = |outer: &str, inner: &str| {
            let mut parts: Vec<String> = (0..left_n)
                .map(|i| format!("{inner}.c{i} AS c{i}"))
                .collect();
            parts.extend((0..right_n).map(|j| format!("{outer}.c{j} AS c{}", left_n + j)));
            parts.join(",")
        };
        let restriction = |key: &str| {
            format!(
                "{}{}{}",
                no_null(key),
                if predicate.is_some() { " AND " } else { "" },
                predicate.as_deref().unwrap_or("")
            )
        };
        let combined = format!(
            "SELECT {},{},l.__n*r.__n AS __m FROM {t0} l JOIN {t1} r ON l.__k=r.__k AND {}",
            left_cols("l"),
            right_cols("r"),
            no_null("l.__k")
        );
        let inner = match predicate {
            Some(p) => format!("SELECT * FROM ({combined}) WHERE {p}"),
            None => combined,
        };
        let unmatched_left = format!(
            "SELECT {},{},l.__n AS __m FROM {t0} l WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE r.__k=l.__k) m WHERE {})",
            left_cols("l"),
            null_cols(left_n, right_n),
            left_pair("l", "r"),
            restriction("l.__k")
        );
        let unmatched_right = format!(
            "SELECT {},{},r.__n AS __m FROM {t1} r WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t0} l WHERE l.__k=r.__k) m WHERE {})",
            null_cols(0, left_n),
            right_cols("r"),
            right_pair("r", "l"),
            restriction("r.__k")
        );
        let body = match *mode {
            "inner" => inner,
            "left" => format!("{inner} UNION ALL {unmatched_left}"),
            "right" => format!("{inner} UNION ALL {unmatched_right}"),
            "full" => format!("{inner} UNION ALL {unmatched_left} UNION ALL {unmatched_right}"),
            "semi" => format!(
                "SELECT {},l.__n AS __m FROM {t0} l WHERE {} AND EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE r.__k=l.__k) m WHERE {})",
                left_cols("l"),
                no_null("l.__k"),
                left_pair("l", "r"),
                restriction("l.__k")
            ),
            _ => format!(
                "SELECT {},l.__n AS __m FROM {t0} l WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE r.__k=l.__k) m WHERE {})",
                left_cols("l"),
                left_pair("l", "r"),
                restriction("l.__k")
            ),
        };
        db.execute_cached(
            &format!("INSERT INTO {out}({cols},__m) SELECT * FROM ({body})"),
            [],
        )?;
        let bad: bool = db
            .prepare_cached(&format!(
                "SELECT EXISTS(SELECT 1 FROM {out} WHERE typeof(__m)!='integer' OR __m<0)"
            ))?
            .query_row([], |r| r.get(0))?;
        if bad {
            return Err(error("join multiplicity overflow"));
        }
        Ok(())
    }
    fn materialize_group(&self, db: &Connection, name: &str, id: usize, restricted: bool) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        let Kind::Group {
            keys,
            expressions,
            order,
            limit,
            offset,
            having,
            window,
        } = &node.kind
        else {
            unreachable!()
        };
        let t = self.source_table(db, name, id, 0, restricted)?;
        let replaced = expressions
            .iter()
            .map(|e| e.replace("__window__", &format!("ORDER BY {}", order.join(","))))
            .collect::<Vec<_>>()
            .join(",");
        if *window || limit.is_some() {
            let peak: i64 = db
                .prepare_cached(&format!("SELECT coalesce(max(__n),0) FROM {t}"))?
                .query_row([], |r| r.get(0))?;
            let expansion = match limit.filter(|n| *n >= 0) {
                Some(n) if !*window => peak.min(n.saturating_add(*offset)),
                _ => peak,
            };
            if expansion > BULK_MULTIPLICITY_BUDGET {
                return Err(error("group multiplicity expansion exceeds budget"));
            }
        }
        if *window {
            // Every partition in one statement: the window runs over the
            // expanded bag partitioned by key, instead of once per key.
            let width = self.nodes[node.inputs[0]].fields.len();
            let cols_in = columns(width);
            let groups: i64 = db
                .prepare_cached(&format!("SELECT count(DISTINCT __k) FROM {t}"))?
                .query_row([], |r| r.get(0))?;
            if groups as usize > BULK_GROUP_BUDGET {
                return Err(error("bulk group budget exceeded"));
            }
            let partitioned = expressions
                .iter()
                .map(|e| e.replacen(" OVER(", " OVER(PARTITION BY __k ", 1))
                .collect::<Vec<_>>()
                .join(",");
            db.execute_cached(
                &format!(
                    "WITH RECURSIVE candidates(__k,{cols_in},__n) AS (SELECT __k,{cols_in},__n FROM {t}),                              copies(__copy) AS (SELECT 1 UNION ALL SELECT __copy+1 FROM copies WHERE __copy<(SELECT coalesce(max(__n),0) FROM candidates))                              INSERT INTO {out}({cols},__m) SELECT q.*,1 FROM (SELECT {partitioned} FROM candidates CROSS JOIN copies WHERE __copy<=__n) q"
                ),
                [],
            )?;
        } else if limit.is_some() {
            let width = self.nodes[node.inputs[0]].fields.len();
            let cols_in = columns(width);
            let wanted = limit.filter(|n| *n >= 0).map(|n| n.saturating_add(*offset));
            // The recursive term walks __copies to 1, so an unclamped __n unrolls every
            // copy before LIMIT applies. A window reads every copy, so only LIMIT clamps.
            let copies = wanted
                .filter(|_| !*window)
                .map(|n| format!("min(__n,{n})"))
                .unwrap_or_else(|| "__n".to_string());
            let candidates = format!(
                "SELECT {cols_in},{copies} FROM {t} WHERE __k=?1{}{}",
                if order.is_empty() {
                    String::new()
                } else {
                    format!(" ORDER BY {}", order.join(","))
                },
                wanted.map(|n| format!(" LIMIT {n}")).unwrap_or_default()
            );
            let single = format!(
                "WITH RECURSIVE candidates({cols_in},__n) AS ({candidates}), copies(__copy) AS (SELECT 1 UNION ALL SELECT __copy+1 FROM copies WHERE __copy<(SELECT coalesce(max(__n),0) FROM candidates)) SELECT {replaced} FROM candidates CROSS JOIN copies WHERE __copy<=__n{}{}",
                if !*window && !order.is_empty() {
                    format!(" ORDER BY {}", order.join(","))
                } else {
                    String::new()
                },
                limit
                    .map(|n| format!(" LIMIT {n} OFFSET {offset}"))
                    .unwrap_or_default()
            );
            let mut statement = db.prepare_cached(&format!(
                "INSERT INTO {out}({cols},__m) SELECT q.*,1 FROM ({single}) q"
            ))?;
            let mut key_statement = db.prepare_cached(&format!("SELECT DISTINCT __k FROM {t}"))?;
            let mut key_rows = key_statement.query([])?;
            let mut groups = 0usize;
            while let Some(key) = key_rows.next()? {
                groups += 1;
                if groups > BULK_GROUP_BUDGET {
                    return Err(error("bulk group budget exceeded"));
                }
                statement.execute([key.get::<_, i64>(0)?])?;
            }
        } else {
            let mut sql =
                format!("INSERT INTO {out}({cols},__m) SELECT {replaced},1 FROM {t}");
            if !keys.is_empty() {
                sql.push_str(" GROUP BY __k");
            }
            if let Some(h) = having {
                sql.push_str(&format!(" HAVING {h}"));
            }
            db.execute_cached(&sql, [])?;
        }
        Ok(())
    }
    fn materialize_fixpoint(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        let Kind::Fixpoint { rules } = &node.kind else {
            unreachable!()
        };
        let member = node.inputs.len();
        let all = table(name, id, member);
        let mut rounds = 0usize;
        let mut lo = max_rowid(db, &all)?;
        for rule in rules {
            self.derive(db, name, id, rule, &all, None)?;
        }
        loop {
            if rounds >= BULK_ROUND_BUDGET {
                return Err(error("fixpoint closure round budget exceeded"));
            }
            rounds += 1;
            let hi = max_rowid(db, &all)?;
            if hi == lo {
                break;
            }
            let mut written = 0usize;
            for rule in rules.iter().filter(|r| r.member().is_some()) {
                written += self.derive(db, name, id, rule, &all, Some((lo, hi)))?;
            }
            if written == 0 {
                break;
            }
            lo = hi;
        }
        db.execute(
            &format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 FROM {all}"),
            [],
        )?;
        Ok(())
    }
    fn derive(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        rule: &Rule,
        target: &str,
        delta: Option<(i64, i64)>,
    ) -> Result<usize> {
        let cols = columns(self.nodes[id].fields.len());
        let mut params = vec![];
        let member = delta
            .map(|(lo, hi)| Role::Range(target.to_string(), lo, hi))
            .unwrap_or_else(|| Role::Table(target.to_string()));
        let from = rule_from(rule, &roles(name, id, 0, rule, None, member), &mut params);
        db.execute_cached(
            &format!(
                "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT {},{} {from}{}",
                rule.key,
                rule.head.join(","),
                rule_where(rule, None)
            ),
            params_from_iter(params),
        )
    }
}
