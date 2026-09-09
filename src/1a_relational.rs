//! Incremental relational operators backed by transactional SQLite arrangements.
use crate::{
    query::{error, quote},
    relational::{Kind, Plan},
};
use rusqlite::{params_from_iter, types::Value, Connection, Result};
use std::collections::BTreeMap;
pub type Row = Vec<Value>;
type Delta = (Row, i64);
fn columns(n: usize) -> String {
    (0..n)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",")
}
fn table(name: &str, id: usize, side: usize) -> String {
    format!("main.{}", quote(&format!("{name}_op{id}_{side}")))
}
fn parameters(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",")
}
fn rows(db: &Connection, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
    let mut s = db.prepare_cached(sql)?;
    let n = s.column_count();
    let result = s
        .query_map(params_from_iter(params), |r| {
            (0..n).map(|i| r.get(i)).collect()
        })?
        .collect();
    result
}
fn key(db: &Connection, row: &[Value]) -> Result<String> {
    let normalized = row
        .iter()
        .map(|v| match v {
            Value::Real(n)
                if n.is_finite()
                    && *n >= i64::MIN as f64
                    && *n < (i64::MAX as f64)
                    && n.fract() == 0.0 =>
            {
                Value::Integer(*n as i64)
            }
            v => v.clone(),
        })
        .collect::<Vec<_>>();
    identity(db, &normalized)
}
fn identity(db: &Connection, row: &[Value]) -> Result<String> {
    let expressions = row
        .iter()
        .enumerate()
        .map(|(i, v)| match v {
            Value::Blob(_) => format!("json_object('blob',hex(?{}))", i + 1),
            Value::Real(_) => format!("json_object('real',?{})", i + 1),
            _ => format!("?{}", i + 1),
        })
        .collect::<Vec<_>>();
    let values = row
        .iter()
        .map(|v| match v {
            Value::Real(n) => Value::Text(format!("{:016x}", n.to_bits())),
            v => v.clone(),
        })
        .collect::<Vec<_>>();
    db.prepare_cached(&format!("SELECT json_array({})", expressions.join(",")))?
        .query_row(params_from_iter(&values), |r| r.get(0))
}
fn evaluate(
    db: &Connection,
    row: &Row,
    expressions: &[String],
    predicate: Option<&str>,
) -> Result<Vec<Row>> {
    if predicate.is_none()
        && expressions.len() == row.len()
        && expressions
            .iter()
            .enumerate()
            .all(|(i, e)| e == &format!("c{i}"))
    {
        return Ok(vec![row.clone()]);
    }
    let fields = (0..row.len())
        .map(|i| format!("?{} AS c{i}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    rows(
        db,
        &format!(
            "SELECT {} FROM (SELECT {fields}){}",
            expressions.join(","),
            predicate.map(|p| format!(" WHERE {p}")).unwrap_or_default()
        ),
        row,
    )
}
fn change(db: &Connection, t: &str, k: &str, row: &Row, d: i64) -> Result<()> {
    let r = identity(db, row)?;
    let old: Option<i64> = db
        .query_row(&format!("SELECT __n FROM {t} WHERE __r=?1"), [&r], |r| {
            r.get(0)
        })
        .optional()?;
    let new = old
        .unwrap_or(0)
        .checked_add(d)
        .ok_or_else(|| error("multiplicity overflow"))?;
    if new < 0 {
        return Err(error("negative arrangement multiplicity"));
    }
    if new == 0 {
        db.execute_cached(&format!("DELETE FROM {t} WHERE __r=?1"), [&r])?;
    } else if old.is_some() {
        db.execute_cached(
            &format!("UPDATE {t} SET __n=?1 WHERE __r=?2"),
            rusqlite::params![new, r],
        )?;
    } else {
        let mut params = vec![Value::Text(k.into()), Value::Text(r), Value::Integer(new)];
        params.extend(row.clone());
        db.execute_cached(
            &format!("INSERT INTO {t} VALUES({})", parameters(params.len())),
            params_from_iter(params),
        )?;
    }
    Ok(())
}
use rusqlite::OptionalExtension;
trait CachedExecute {
    fn execute_cached<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize>;
}
impl CachedExecute for Connection {
    fn execute_cached<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize> {
        self.prepare_cached(sql)?.execute(params)
    }
}
fn differences(db: &Connection, before: Vec<Delta>, after: Vec<Delta>) -> Result<Vec<Delta>> {
    let mut values = BTreeMap::<String, Delta>::new();
    for (rows, sign) in [(before, -1), (after, 1)] {
        for (row, n) in rows {
            let k = identity(db, &row)?;
            let entry = values.entry(k).or_insert((row, 0));
            entry.1 = entry
                .1
                .checked_add(
                    n.checked_mul(sign)
                        .ok_or_else(|| error("multiplicity overflow"))?,
                )
                .ok_or_else(|| error("multiplicity overflow"))?;
        }
    }
    Ok(values.into_values().filter(|(_, n)| *n != 0).collect())
}
fn weighted(db: &Connection, sql: &str, k: &str) -> Result<Vec<Delta>> {
    rows(db, sql, &[Value::Text(k.into())])?
        .into_iter()
        .map(|mut r| match r.pop() {
            Some(Value::Integer(n)) => Ok((r, n)),
            _ => Err(error("invalid multiplicity")),
        })
        .collect()
}
impl Plan {
    pub fn create_state(&self, db: &Connection, name: &str) -> Result<Vec<(&'static str, String)>> {
        let mut objects = vec![];
        let state = format!("{name}_state");
        db.execute_batch(&format!(
            "CREATE TABLE main.{}(__key TEXT NOT NULL,{}); CREATE INDEX main.{} ON {}(__key)",
            quote(&state),
            (0..self.names.len())
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>()
                .join(","),
            quote(&format!("__ivm_{name}_result_key")),
            quote(&state)
        ))?;
        objects.push(("table", state));
        objects.push(("index", format!("__ivm_{name}_result_key")));
        for (id, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind, Kind::Input(_) | Kind::Map { .. }) {
                continue;
            }
            for (side, input) in node.inputs.iter().enumerate() {
                let t = format!("{name}_op{id}_{side}");
                let n = self.nodes[*input].fields.len();
                db.execute_batch(&format!("CREATE TABLE main.{}(__k TEXT NOT NULL,__r TEXT NOT NULL UNIQUE,__n INTEGER NOT NULL,{}); CREATE INDEX main.{} ON {}(__k)",quote(&t),columns(n),quote(&format!("__ivm_{name}_op{id}_{side}_key")),quote(&t)))?;
                objects.push(("table", t));
                objects.push(("index", format!("__ivm_{name}_op{id}_{side}_key")));
                if let Kind::Group {
                    order,
                    limit: Some(_),
                    ..
                } = &node.kind
                {
                    if !order.is_empty() {
                        let index = format!("__ivm_{name}_op{id}_{side}_order");
                        db.execute_batch(&format!(
                            "CREATE INDEX main.{} ON {}(__k,{})",
                            quote(&index),
                            quote(&format!("{name}_op{id}_{side}")),
                            order
                                .iter()
                                .map(|o| o.replace(" NULLS FIRST", "").replace(" NULLS LAST", ""))
                                .collect::<Vec<_>>()
                                .join(",")
                        ))?;
                        objects.push(("index", index));
                    }
                }
            }
            if matches!(node.kind, Kind::Reach) {
                let t = format!("{name}_op{id}_2");
                db.execute_batch(&format!("CREATE TABLE main.{}(__k TEXT NOT NULL,__r TEXT NOT NULL UNIQUE,__n INTEGER NOT NULL,c0)",quote(&t)))?;
                objects.push(("table", t));
                for (side, column) in [(0, 0), (1, 0), (1, 1), (2, 0)] {
                    let index = format!("__ivm_{name}_reach{id}_{side}_{column}");
                    db.execute_batch(&format!(
                        "CREATE INDEX main.{} ON {}(c{column} COLLATE {})",
                        quote(&index),
                        quote(&format!("{name}_op{id}_{side}")),
                        node.fields[0].collation
                    ))?;
                    objects.push(("index", index));
                }
            }
        }
        Ok(objects)
    }
    pub fn populate(&self, db: &Connection, name: &str) -> Result<()> {
        // Global aggregates have an output even before the first source row.
        // Seed downstream first, then propagate upstream empty outputs.
        for (id, node) in self.nodes.iter().enumerate().rev() {
            if let Kind::Group {
                keys,
                expressions,
                limit: None,
                window: false,
                having,
                ..
            } = &node.kind
            {
                if keys.is_empty() {
                    for row in rows(
                        db,
                        &format!(
                            "SELECT {} FROM {} WHERE __k='[]'{}",
                            expressions.join(","),
                            table(name, id, 0),
                            having
                                .as_ref()
                                .map(|h| format!(" HAVING {h}"))
                                .unwrap_or_default()
                        ),
                        &[],
                    )? {
                        self.emit(db, name, id, row, 1)?;
                    }
                }
            }
        }
        for (source, s) in self.sources.iter().enumerate() {
            // Source readback is materialized before updates reuse this connection.
            for row in rows(
                db,
                &format!(
                    "SELECT {} FROM main.{}",
                    s.columns
                        .iter()
                        .map(|c| quote(c))
                        .collect::<Vec<_>>()
                        .join(","),
                    quote(&s.name)
                ),
                &[],
            )? {
                self.input(db, name, source, row, 1)?;
            }
        }
        Ok(())
    }
    pub fn input(
        &self,
        db: &Connection,
        name: &str,
        source: usize,
        row: Row,
        d: i64,
    ) -> Result<()> {
        for (value, affinity) in row.iter().zip(&self.sources[source].affinities) {
            let valid = match value {
                Value::Null => true,
                Value::Blob(_) => affinity.is_empty(),
                Value::Text(_) => affinity == "TEXT" || affinity.is_empty(),
                Value::Integer(_) => affinity != "TEXT",
                Value::Real(_) => affinity != "TEXT" && affinity != "INTEGER",
            };
            if !valid {
                return Err(error("source value does not conform to declared affinity"));
            }
        }
        for (id, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind,Kind::Input(s) if s==source) {
                self.emit(db, name, id, row.clone(), d)?;
            }
        }
        Ok(())
    }
    fn emit(&self, db: &Connection, name: &str, id: usize, row: Row, d: i64) -> Result<()> {
        if d == 0 {
            return Ok(());
        }
        if id == self.output {
            let state = format!("main.{}", quote(&format!("{name}_state")));
            let k = identity(db, &row)?;
            if d > 0 {
                let mut params = vec![Value::Text(k)];
                params.extend(row.clone());
                let mut statement = db.prepare_cached(&format!(
                    "INSERT INTO {state} VALUES({})",
                    parameters(params.len())
                ))?;
                for _ in 0..d {
                    statement.execute(params_from_iter(&params))?;
                }
            } else {
                let changed=db.execute_cached(&format!("DELETE FROM {state} WHERE rowid IN(SELECT rowid FROM {state} WHERE __key=?1 LIMIT ?2)"),rusqlite::params![k,-d])?;
                if changed as i64 != -d {
                    return Err(error("missing result multiplicity"));
                }
            }
        }
        for (next, node) in self.nodes.iter().enumerate() {
            for (side, input) in node.inputs.iter().enumerate() {
                if *input == id {
                    for (out, diff) in self.apply(db, name, next, side, &row, d)? {
                        self.emit(db, name, next, out, diff)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn apply(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        side: usize,
        row: &Row,
        d: i64,
    ) -> Result<Vec<Delta>> {
        let node = &self.nodes[id];
        match &node.kind {
            Kind::Input(_) => unreachable!(),
            Kind::Map {
                expressions,
                predicate,
            } => Ok(evaluate(db, row, expressions, predicate.as_deref())?
                .into_iter()
                .map(|r| (r, d))
                .collect()),
            Kind::Set(op) => {
                if *op == "all" {
                    return Ok(vec![(row.clone(), d)]);
                }
                let normalized = node
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| crate::relational::key_expression(&format!("c{i}"), &f.collation))
                    .collect::<Vec<_>>();
                let k = key(db, &evaluate(db, row, &normalized, None)?.remove(0))?;
                let count = |side| -> Result<i64> {
                    db.query_row(
                        &format!(
                            "SELECT coalesce(sum(__n),0) FROM {} WHERE __k=?1",
                            table(name, id, side)
                        ),
                        [&k],
                        |r| r.get(0),
                    )
                };
                let present = |l: i64, r: i64| match *op {
                    "distinct" => l > 0,
                    "union" => l + r > 0,
                    "except" => l > 0 && r == 0,
                    _ => l > 0 && r > 0,
                };
                let snapshot = || -> Result<Vec<Delta>> {
                    let l = count(0)?;
                    let r = if node.inputs.len() == 2 { count(1)? } else { 0 };
                    if !present(l, r) {
                        return Ok(vec![]);
                    }
                    let side = if l > 0 { 0 } else { 1 };
                    Ok(rows(
                        db,
                        &format!(
                            "SELECT {} FROM {} WHERE __k=?1 ORDER BY rowid LIMIT 1",
                            columns(row.len()),
                            table(name, id, side)
                        ),
                        &[Value::Text(k.clone())],
                    )?
                    .into_iter()
                    .map(|r| (r, 1))
                    .collect())
                };
                let before = snapshot()?;
                change(db, &table(name, id, side), &k, row, d)?;
                differences(db, before, snapshot()?)
            }
            Kind::Join {
                left,
                right,
                mode,
                predicate,
            } => {
                let positions = if side == 0 { left } else { right };
                let expressions = positions
                    .iter()
                    .map(|i| {
                        crate::relational::key_expression(
                            &format!("c{i}"),
                            &self.nodes[node.inputs[side]].fields[*i].collation,
                        )
                    })
                    .collect::<Vec<_>>();
                let values = if positions.is_empty() {
                    vec![]
                } else {
                    evaluate(db, row, &expressions, None)?.remove(0)
                };
                let null = values.contains(&Value::Null);
                let k = key(db, &values)?;
                let left_n = self.nodes[node.inputs[0]].fields.len();
                let right_n = self.nodes[node.inputs[1]].fields.len();
                if *mode == "inner" {
                    let other = table(name, id, 1 - side);
                    let n = if side == 0 { right_n } else { left_n };
                    let matches = if null {
                        vec![]
                    } else {
                        weighted(
                            db,
                            &format!("SELECT {},__n FROM {other} WHERE __k=?1", columns(n)),
                            &k,
                        )?
                    };
                    change(db, &table(name, id, side), &k, row, d)?;
                    let mut result = vec![];
                    for (other, n) in matches {
                        let mut output = if side == 0 {
                            row.clone()
                        } else {
                            other.clone()
                        };
                        output.extend(if side == 0 { other } else { row.clone() });
                        if predicate
                            .as_ref()
                            .map(|p| {
                                evaluate(db, &output, &["1".into()], Some(p)).map(|r| !r.is_empty())
                            })
                            .transpose()?
                            .unwrap_or(true)
                        {
                            result.push((
                                output,
                                d.checked_mul(n)
                                    .ok_or_else(|| error("join multiplicity overflow"))?,
                            ));
                        }
                    }
                    return Ok(result);
                }
                let snapshot = || -> Result<Vec<Delta>> {
                    let l = weighted(
                        db,
                        &format!(
                            "SELECT {},__n FROM {} WHERE __k=?1",
                            columns(left_n),
                            table(name, id, 0)
                        ),
                        &k,
                    )?;
                    let r = weighted(
                        db,
                        &format!(
                            "SELECT {},__n FROM {} WHERE __k=?1",
                            columns(right_n),
                            table(name, id, 1)
                        ),
                        &k,
                    )?;
                    let mut result = vec![];
                    let mut right_matched = vec![false; r.len()];
                    for (a, na) in &l {
                        let mut matched = false;
                        if !null {
                            for (j, (b, nb)) in r.iter().enumerate() {
                                let mut out = a.clone();
                                out.extend(b.clone());
                                if predicate
                                    .as_ref()
                                    .map(|p| {
                                        evaluate(db, &out, &["1".into()], Some(p))
                                            .map(|r| !r.is_empty())
                                    })
                                    .transpose()?
                                    .unwrap_or(true)
                                {
                                    matched = true;
                                    right_matched[j] = true;
                                    if !["semi", "anti"].contains(mode) {
                                        result.push((
                                            out,
                                            na.checked_mul(*nb).ok_or_else(|| {
                                                error("join multiplicity overflow")
                                            })?,
                                        ));
                                    }
                                }
                            }
                        }
                        if *mode == "semi" && matched || *mode == "anti" && !matched {
                            result.push((a.clone(), *na));
                        }
                        if !matched && ["left", "full"].contains(mode) {
                            let mut out = a.clone();
                            out.extend(vec![Value::Null; right_n]);
                            result.push((out, *na));
                        }
                    }
                    if ["right", "full"].contains(mode) {
                        for (j, (b, nb)) in r.iter().enumerate() {
                            if !right_matched[j] {
                                let mut out = vec![Value::Null; left_n];
                                out.extend(b.clone());
                                result.push((out, *nb));
                            }
                        }
                    }
                    Ok(result)
                };
                let before = snapshot()?;
                change(db, &table(name, id, side), &k, row, d)?;
                differences(db, before, snapshot()?)
            }
            Kind::Group {
                keys,
                expressions,
                order,
                limit,
                offset,
                having,
                window,
            } => {
                let t = table(name, id, 0);
                let k = if keys.is_empty() {
                    "[]".into()
                } else {
                    key(db, &evaluate(db, row, keys, None)?.remove(0))?
                };
                let snapshot = || -> Result<Vec<Delta>> {
                    let present: bool = db.query_row(
                        &format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE __k=?1)"),
                        [&k],
                        |r| r.get(0),
                    )?;
                    if !present && (!keys.is_empty() || *window || limit.is_some()) {
                        return Ok(vec![]);
                    }
                    let expressions = expressions
                        .iter()
                        .map(|e| e.replace("__window__", &format!("ORDER BY {}", order.join(","))))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = if *window || limit.is_some() {
                        let width = self.nodes[node.inputs[0]].fields.len();
                        let cols = columns(width);
                        // At most k distinct positive-support rows can contribute
                        // to the first k bag rows. The ordered index bounds reads.
                        let candidates = format!(
                            "SELECT {cols},__n FROM {t} WHERE __k=?1{}{}",
                            if !order.is_empty() {
                                format!(" ORDER BY {}", order.join(","))
                            } else {
                                String::new()
                            },
                            limit
                                .filter(|n| *n >= 0)
                                .map(|n| n.saturating_add(*offset))
                                .map(|n| format!(" LIMIT {n}"))
                                .unwrap_or_default()
                        );
                        format!("WITH RECURSIVE candidates AS ({candidates}), expanded({cols},__copies) AS (SELECT {cols},__n FROM candidates UNION ALL SELECT {cols},__copies-1 FROM expanded WHERE __copies>1) SELECT {expressions} FROM expanded{}{}",
                            if !*window&&!order.is_empty(){format!(" ORDER BY {}",order.join(","))}else{String::new()},limit.map(|n|format!(" LIMIT {n} OFFSET {offset}")).unwrap_or_default())
                    } else {
                        format!(
                            "SELECT {expressions} FROM {t} WHERE __k=?1{}{}",
                            if keys.is_empty() { "" } else { " GROUP BY __k" },
                            having
                                .as_ref()
                                .map(|h| format!(" HAVING {h}"))
                                .unwrap_or_default()
                        )
                    };
                    Ok(rows(db, &sql, &[Value::Text(k.clone())])?
                        .into_iter()
                        .map(|r| (r, 1))
                        .collect())
                };
                let before = snapshot()?;
                change(db, &t, &k, row, d)?;
                differences(db, before, snapshot()?)
            }
            Kind::Reach => self.reach(db, name, id, side, row, d),
        }
    }
    fn reach(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        side: usize,
        row: &Row,
        d: i64,
    ) -> Result<Vec<Delta>> {
        let roots = table(name, id, 0);
        let edges = table(name, id, 1);
        let reached = table(name, id, 2);
        let collation = &self.nodes[id].fields[0].collation;
        change(db, &table(name, id, side), &key(db, &row[..1])?, row, d)?;
        let existing = |v: &Value| -> Result<Option<Value>> {
            db.query_row(
                &format!("SELECT c0 FROM {reached} WHERE c0 COLLATE {collation} IS ?1 LIMIT 1"),
                [v],
                |r| r.get(0),
            )
            .optional()
        };
        let contains = |v: &Value| -> Result<bool> { Ok(existing(v)?.is_some()) };
        if side == 1 && (row[0] == Value::Null || !contains(&row[0])?) {
            return Ok(vec![]);
        }
        let start = row[if side == 0 { 0 } else { 1 }].clone();
        let successors = |v: &Value| -> Result<Vec<Value>> {
            if *v == Value::Null {
                return Ok(vec![]);
            }
            Ok(rows(
                db,
                &format!("SELECT DISTINCT c1 FROM {edges} WHERE c0 COLLATE {collation}=?1"),
                &[v.clone()],
            )?
            .into_iter()
            .map(|r| r[0].clone())
            .collect())
        };
        let mut before = BTreeMap::<String, Value>::new();
        let mut seeds = vec![];
        if d < 0 {
            let mut todo = vec![start];
            while let Some(v) = todo.pop() {
                let Some(v) = existing(&v)? else {
                    continue;
                };
                let k = key(db, &[v.clone()])?;
                if before.contains_key(&k) {
                    continue;
                }
                before.insert(k, v.clone());
                todo.extend(successors(&v)?);
            }
            for (k, v) in &before {
                change(db, &reached, k, &vec![v.clone()], -1)?;
            }
            for v in before.values() {
                let rooted:bool=db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {roots} WHERE c0 COLLATE {collation} IS ?1) OR EXISTS(SELECT 1 FROM {edges} e JOIN {reached} r ON e.c0 COLLATE {collation}=r.c0 WHERE e.c1 COLLATE {collation} IS ?1)"),[v],|r|r.get(0))?;
                if rooted {
                    seeds.push(v.clone());
                }
            }
        } else {
            seeds.push(start);
        }
        let mut after = BTreeMap::<String, Value>::new();
        while let Some(v) = seeds.pop() {
            if contains(&v)? {
                continue;
            }
            let k = key(db, &[v.clone()])?;
            change(db, &reached, &k, &vec![v.clone()], 1)?;
            after.insert(k, v.clone());
            seeds.extend(successors(&v)?);
        }
        let mut result = vec![];
        for (k, v) in &before {
            if !after.contains_key(k) {
                result.push((vec![v.clone()], -1));
            }
        }
        for (k, v) in after {
            if !before.contains_key(&k) {
                result.push((vec![v], 1));
            }
        }
        Ok(result)
    }
}

pub fn install(db: &Connection, name: &str, sql: &str, plan: &Plan) -> Result<()> {
    crate::maintenance::validate_name(name)?;
    let settings:bool=db.query_row("SELECT (SELECT recursive_triggers FROM pragma_recursive_triggers)=1 AND (SELECT trusted_schema FROM pragma_trusted_schema)=1",[],|r|r.get(0))?;
    if !settings {
        return Err(error(
            "sqlite_ivm requires recursive_triggers=ON and trusted_schema=ON",
        ));
    }
    let mut objects = plan.create_state(db, name)?;
    objects.extend(hooks(db, name, plan)?);
    let columns = plan
        .sources
        .iter()
        .enumerate()
        .flat_map(|(source, s)| {
            s.columns.iter().map(move |name| crate::query::Column {
                source,
                name: name.clone(),
            })
        })
        .collect::<Vec<_>>();
    crate::catalog::record_objects(
        db,
        name,
        sql,
        &plan
            .sources
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
        &columns.iter().collect::<Vec<_>>(),
        &objects,
    )?;
    plan.populate(db, name)
}
pub fn hooks(db: &Connection, name: &str, plan: &Plan) -> Result<Vec<(&'static str, String)>> {
    let mut objects = vec![];
    for (source, s) in plan.sources.iter().enumerate() {
        for event in ["INSERT", "DELETE", "UPDATE"] {
            let trigger = format!("__ivm_{name}_{source}_{}_after", event.to_ascii_lowercase());
            let images = match event {
                "INSERT" => vec![("NEW", 1)],
                "DELETE" => vec![("OLD", 0)],
                _ => vec![("OLD", 0), ("NEW", 1)],
            };
            let mut body=String::from("SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)!=1 THEN RAISE(ABORT,'sqlite_ivm requires recursive_triggers=ON') END;");
            for (image, adding) in images {
                body.push_str(&format!("INSERT INTO {}(__ivm_source,__ivm_adding,__ivm_row) VALUES({source},{adding},json_array({}));",quote(name),s.columns.iter().map(|c|{
                let value=format!("{image}.{}",quote(c));
                format!("json_array(typeof({value}),CASE typeof({value}) WHEN 'blob' THEN hex({value}) WHEN 'real' THEN CASE WHEN {value}>1.7976931348623157e308 THEN '9e999' WHEN {value}< -1.7976931348623157e308 THEN '-9e999' ELSE printf('%!.17g',{value}) END ELSE {value} END)")
            }).collect::<Vec<_>>().join(",")));
            }
            // Arrangements contain the old row independently of the source SQL
            // table. AFTER avoids retracting writes rejected by OR IGNORE.
            db.execute_batch(&format!(
                "CREATE TRIGGER main.{} AFTER {event} ON {} BEGIN {body} END",
                quote(&trigger),
                quote(&s.name)
            ))?;
            objects.push(("trigger", trigger));
        }
    }
    Ok(objects)
}
