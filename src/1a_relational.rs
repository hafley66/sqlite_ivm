//! Every operator emits the row stored in its arrangement.
//! Equality keys select membership, while stored row values select emitted identity.
use crate::{
    query::{error, quote},
    relational::{Kind, Occurrence, Plan, Rule},
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
fn change(db: &Connection, t: &str, k: &str, row: &Row, d: i64) -> Result<(i64, i64)> {
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
    Ok((old.unwrap_or(0), new))
}
fn max_rowid(db: &Connection, t: &str) -> Result<i64> {
    db.prepare_cached(&format!("SELECT coalesce(max(rowid),0) FROM {t}"))?
        .query_row([], |r| r.get(0))
}
enum Role<'a> {
    Table(String),
    Params(&'a Row),
    Range(String, i64, i64),
}
/// Every occurrence becomes a flattenable subquery renaming its columns into
/// the rule's shared `c{i}` namespace; `?` numbering continues from `params`.
fn rule_from(rule: &Rule, roles: &[Role<'_>], params: &mut Vec<Value>) -> String {
    let mut offset = 0;
    let mut sources = vec![];
    for (n, ((_, width), role)) in rule.occurrences.iter().zip(roles).enumerate() {
        let renamed = |prefix: &str| {
            (0..*width)
                .map(|i| format!("{prefix}c{i} AS c{}", offset + i))
                .collect::<Vec<_>>()
                .join(",")
        };
        sources.push(match role {
            Role::Table(t) => format!("(SELECT {} FROM {t}) q{n}", renamed("")),
            Role::Params(row) => {
                let values = row
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        params.push(v.clone());
                        format!("?{} AS c{}", params.len(), offset + i)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!("(SELECT {values}) q{n}")
            }
            Role::Range(t, lo, hi) => {
                params.push(Value::Integer(*lo));
                params.push(Value::Integer(*hi));
                format!(
                    "(SELECT {} FROM {t} WHERE rowid>?{} AND rowid<=?{}) q{n}",
                    renamed(""),
                    params.len() - 1,
                    params.len()
                )
            }
        });
        offset += width;
    }
    format!("FROM {}", sources.join(","))
}
fn roles<'a>(
    name: &str,
    id: usize,
    side: usize,
    rule: &Rule,
    bound: Option<&'a Row>,
    member: Role<'a>,
) -> Vec<Role<'a>> {
    rule.occurrences
        .iter()
        .map(|(o, _)| match (o, bound) {
            (Occurrence::Input(s), Some(row)) if *s == side => Role::Params(row),
            (Occurrence::Input(s), _) => Role::Table(table(name, id, *s)),
            (Occurrence::Member, _) => match &member {
                Role::Table(t) => Role::Table(t.clone()),
                Role::Range(t, lo, hi) => Role::Range(t.clone(), *lo, *hi),
                Role::Params(row) => Role::Params(row),
            },
        })
        .collect()
}
fn rule_where(rule: &Rule, extra: Option<&str>) -> String {
    match (&rule.predicate, extra) {
        (None, None) => String::new(),
        (Some(p), None) => format!(" WHERE {p}"),
        (None, Some(e)) => format!(" WHERE {e}"),
        (Some(p), Some(e)) => format!(" WHERE {p} AND {e}"),
    }
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
const BULK_ROUND_BUDGET: usize = 100_000;
const BULK_GROUP_BUDGET: usize = 100_000;
const BULK_MULTIPLICITY_BUDGET: i64 = 1_000_000;
fn out_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_out_{width}_{id}")
}
fn json_key(parts: Vec<String>) -> String {
    format!("json_array({})", parts.join(","))
}
fn folded(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN CASE WHEN {value}=CAST({value} AS INTEGER) AND typeof(CAST({value} AS INTEGER))='integer' THEN CAST({value} AS INTEGER) ELSE json_object('real',sqlite_ivm_real_hex({value})) END ELSE {value} END")
}
fn plain(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN json_object('real',sqlite_ivm_real_hex({value})) ELSE {value} END")
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
            if let Kind::Fixpoint { rules } = &node.kind {
                let member = node.inputs.len();
                for side in [member, member + 1] {
                    let t = format!("{name}_op{id}_{side}");
                    db.execute_batch(&format!(
                        "CREATE TABLE main.{}(__k TEXT NOT NULL UNIQUE,{})",
                        quote(&t),
                        columns(node.fields.len())
                    ))?;
                    objects.push(("table", t));
                }
                let mut created = vec![];
                for (occurrence, expression) in rules.iter().flat_map(|r| &r.indexes) {
                    let side = match occurrence {
                        Occurrence::Input(side) => *side,
                        Occurrence::Member => member,
                    };
                    if created.contains(&(side, expression)) {
                        continue;
                    }
                    let index = format!("__ivm_{name}_fix{id}_{}", created.len());
                    db.execute_batch(&format!(
                        "CREATE INDEX main.{} ON {}({expression})",
                        quote(&index),
                        quote(&format!("{name}_op{id}_{side}"))
                    ))?;
                    objects.push(("index", index));
                    created.push((side, expression));
                }
            }
        }
        Ok(objects)
    }
    pub fn populate(&self, db: &Connection, name: &str) -> Result<()> {
        for source in &self.sources {
            let bad = source
                .columns
                .iter()
                .zip(&source.affinities)
                .filter_map(|(column, affinity)| {
                    let c = quote(column);
                    match affinity.as_str() {
                        "TEXT" => Some(format!("typeof({c}) IN ('blob','integer','real')")),
                        "INTEGER" => Some(format!("typeof({c}) IN ('blob','text','real')")),
                        "REAL" | "NUMERIC" => Some(format!("typeof({c}) IN ('blob','text')")),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            if bad.is_empty() {
                continue;
            }
            let invalid: bool = db.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM main.{} WHERE {})",
                    quote(&source.name),
                    bad.join(" OR ")
                ),
                [],
                |r| r.get(0),
            )?;
            if invalid {
                return Err(error("source value does not conform to declared affinity"));
            }
        }
        for (id, node) in self.nodes.iter().enumerate() {
            let out = out_table(id, node.fields.len());
            db.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {out}({},__m)",
                    columns(node.fields.len())
                ),
                [],
            )?;
            db.execute(&format!("DELETE FROM {out}"), [])?;
        }
        let mut reads = vec![0usize; self.nodes.len()];
        reads[self.output] += 1;
        for node in &self.nodes {
            match &node.kind {
                Kind::Input(_) => {}
                Kind::Map { .. } => reads[node.inputs[0]] += 1,
                Kind::Set(op) if *op == "all" => reads[node.inputs[0]] += 1,
                _ => {
                    for side in 0..node.inputs.len() {
                        reads[node.inputs[side]] += 1;
                    }
                }
            }
        }
        for id in 0..self.nodes.len() {
            let node = &self.nodes[id];
            let direct = match &node.kind {
                Kind::Map { .. } => false,
                Kind::Set(op) if *op == "all" => false,
                _ => true,
            };
            if direct {
                for side in 0..node.inputs.len() {
                    self.fill(db, name, id, side)?;
                    self.exhaust(db, &mut reads, node.inputs[side])?;
                }
            }
            self.materialize(db, name, id)?;
            if !direct {
                self.exhaust(db, &mut reads, node.inputs[0])?;
            }
            if id == self.output {
                self.write_state(db, name, id)?;
                self.exhaust(db, &mut reads, id)?;
            }
        }
        Ok(())
    }
    fn exhaust(&self, db: &Connection, reads: &mut [usize], child: usize) -> Result<()> {
        reads[child] -= 1;
        if reads[child] == 0 {
            let node = &self.nodes[child];
            db.execute(
                &format!("DELETE FROM {}", out_table(child, node.fields.len())),
                [],
            )?;
        }
        Ok(())
    }
    fn fill(&self, db: &Connection, name: &str, id: usize, side: usize) -> Result<()> {
        let node = &self.nodes[id];
        let child = node.inputs[side];
        let child_node = &self.nodes[child];
        let width = child_node.fields.len();
        let out = out_table(child, width);
        let t = table(name, id, side);
        let r = json_key((0..width).map(|i| plain(&format!("c{i}"))).collect());
        let k = match &node.kind {
            Kind::Set(_) => json_key(
                (0..node.fields.len())
                    .map(|i| {
                        folded(&crate::relational::key_expression(
                            &format!("c{i}"),
                            &node.fields[i].collation,
                        ))
                    })
                    .collect(),
            ),
            Kind::Join { left, right, .. } => {
                let positions = if side == 0 { left } else { right };
                json_key(
                    positions
                        .iter()
                        .map(|i| {
                            folded(&crate::relational::key_expression(
                                &format!("c{i}"),
                                &child_node.fields[*i].collation,
                            ))
                        })
                        .collect(),
                )
            }
            Kind::Group { keys, .. } => json_key(keys.iter().map(|e| folded(e)).collect()),
            Kind::Fixpoint { .. } => {
                json_key((0..width).map(|i| folded(&format!("c{i}"))).collect())
            }
            _ => return Ok(()),
        };
        db.execute(
            &format!(
                "INSERT INTO {t}(__k,__r,__n,{}) SELECT {k},{r},__m,c0{} FROM {out} WHERE 1 ON CONFLICT(__r) DO UPDATE SET __n=__n+excluded.__n",
                columns(width),
                (1..width).map(|i| format!(",c{i}")).collect::<String>()
            ),
            [],
        )?;
        let bad: bool = db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE typeof(__n)!='integer' OR __n<0)"),
            [],
            |r| r.get(0),
        )?;
        if bad {
            return Err(error("arrangement multiplicity overflow"));
        }
        Ok(())
    }
    fn materialize(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        match &node.kind {
            Kind::Input(source) => {
                let s = &self.sources[*source];
                db.execute(
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
            }
            Kind::Map {
                expressions,
                predicate,
            } => {
                let child = out_table(node.inputs[0], self.nodes[node.inputs[0]].fields.len());
                db.execute(
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
            }
            Kind::Set(op) => {
                let width = node.fields.len();
                let reps = |t: &str| {
                    format!(
                        "SELECT a.c0{},1 FROM {t} a WHERE a.rowid=(SELECT MIN(rowid) FROM {t} b WHERE b.__k=a.__k)",
                        (1..width).map(|i| format!(",a.c{i}")).collect::<String>()
                    )
                };
                let t0 = table(name, id, 0);
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
                        table(name, id, 1),
                        table(name, id, 1),
                        t0
                    ),
                    ("except", _) => format!(
                        "INSERT INTO {out}({cols},__m) {} AND a.__k NOT IN(SELECT __k FROM {})",
                        reps(&t0),
                        table(name, id, 1)
                    ),
                    _ => format!(
                        "INSERT INTO {out}({cols},__m) {} AND a.__k IN(SELECT __k FROM {})",
                        reps(&t0),
                        table(name, id, 1)
                    ),
                };
                db.execute(&sql, [])?;
            }
            Kind::Join {
                left: _,
                right: _,
                mode,
                predicate,
            } => {
                let left_n = self.nodes[node.inputs[0]].fields.len();
                let right_n = self.nodes[node.inputs[1]].fields.len();
                let t0 = table(name, id, 0);
                let t1 = table(name, id, 1);
                let no_null = |k: &str| {
                    format!(
                        "NOT EXISTS(SELECT 1 FROM json_each({k}) WHERE json_type(value)='null')"
                    )
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
                db.execute(
                    &format!("INSERT INTO {out}({cols},__m) SELECT * FROM ({body})"),
                    [],
                )?;
                let bad: bool = db.query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM {out} WHERE typeof(__m)!='integer' OR __m<0)"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                if bad {
                    return Err(error("join multiplicity overflow"));
                }
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
                let replaced = expressions
                    .iter()
                    .map(|e| e.replace("__window__", &format!("ORDER BY {}", order.join(","))))
                    .collect::<Vec<_>>()
                    .join(",");
                if *window || limit.is_some() {
                    let groups: i64 =
                        db.query_row(&format!("SELECT count(DISTINCT __k) FROM {t}"), [], |r| {
                            r.get(0)
                        })?;
                    if groups as usize > BULK_GROUP_BUDGET {
                        return Err(error("bulk group budget exceeded"));
                    }
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
                        "WITH RECURSIVE candidates({cols_in},__n) AS ({candidates}), expanded({cols_in},__copies) AS (SELECT {cols_in},__n FROM candidates UNION ALL SELECT {cols_in},__copies-1 FROM expanded WHERE __copies>1) SELECT {replaced} FROM expanded{}{}",
                        if !*window && !order.is_empty() {
                            format!(" ORDER BY {}", order.join(","))
                        } else {
                            String::new()
                        },
                        limit
                            .map(|n| format!(" LIMIT {n} OFFSET {offset}"))
                            .unwrap_or_default()
                    );
                    let mut statement = db.prepare(&format!(
                        "INSERT INTO {out}({cols},__m) SELECT q.*,1 FROM ({single}) q"
                    ))?;
                    let mut key_statement = db.prepare(&format!("SELECT DISTINCT __k FROM {t}"))?;
                    let mut key_rows = key_statement.query([])?;
                    while let Some(key) = key_rows.next()? {
                        let key: String = key.get(0)?;
                        statement.execute([&key])?;
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
                    db.execute(&sql, [])?;
                }
            }
            Kind::Fixpoint { rules } => {
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
            }
        }
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
    fn write_state(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let width = self.nodes[id].fields.len();
        let out = out_table(id, width);
        let state = format!("main.{}", quote(&format!("{name}_state")));
        let peak: i64 = db.query_row(
            &format!("SELECT coalesce(max(__m),0) FROM {out}"),
            [],
            |r| r.get(0),
        )?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        let k = json_key((0..width).map(|i| plain(&format!("o.c{i}"))).collect());
        db.execute(
            &format!(
                "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce((SELECT max(__m) FROM {out}),0))) INSERT INTO {state}(__key,{}) SELECT {k},o.c0{} FROM {out} o,seq WHERE seq.n<=o.__m",
                columns(width),
                (1..width).map(|i| format!(",o.c{i}")).collect::<String>()
            ),
            [],
        )?;
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
        let _span = tracing::debug_span!(
            "maintain",
            view = name,
            source = self.sources[source].name.as_str(),
            sign = d.signum()
        )
        .entered();
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
                        let wanted = limit.filter(|n| *n >= 0).map(|n| n.saturating_add(*offset));
                        let copies = wanted
                            .filter(|_| !*window)
                            .map(|n| format!("min(__n,{n}) AS __n"))
                            .unwrap_or_else(|| "__n".to_string());
                        let candidates = format!(
                            "SELECT {cols},{copies} FROM {t} WHERE __k=?1{}{}",
                            if !order.is_empty() {
                                format!(" ORDER BY {}", order.join(","))
                            } else {
                                String::new()
                            },
                            wanted.map(|n| format!(" LIMIT {n}")).unwrap_or_default()
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
            Kind::Fixpoint { rules } => self.fixpoint(db, name, id, side, row, d, rules),
        }
    }
    /// Delete-and-rederive over one member set. Insertion and rederivation run
    /// semi-naive rounds whose delta is a rowid range of the member table.
    #[allow(clippy::too_many_arguments)]
    fn fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        side: usize,
        row: &Row,
        d: i64,
        rules: &[Rule],
    ) -> Result<Vec<Delta>> {
        let span = tracing::debug_span!(
            "fixpoint",
            view = name,
            node = id,
            rows_in = d.abs(),
            rounds = tracing::field::Empty
        )
        .entered();
        let round_count = std::cell::Cell::new(0usize);
        let round_span = |phase: &'static str| {
            let index = round_count.get();
            round_count.set(index + 1);
            span.record("rounds", index + 1);
            tracing::debug_span!("round", phase, index, rows = tracing::field::Empty).entered()
        };
        let node = &self.nodes[id];
        let (old, new) = change(db, &table(name, id, side), &key(db, row)?, row, d)?;
        if (old > 0) == (new > 0) {
            return Ok(vec![]);
        }
        let width = node.fields.len();
        let cols = columns(width);
        let all = table(name, id, node.inputs.len());
        let work = table(name, id, node.inputs.len() + 1);
        let derive = |target: &str,
                      rule: &Rule,
                      roles: &[Role<'_>],
                      only_present: bool|
         -> Result<usize> {
            let mut params = vec![];
            let from = rule_from(rule, roles, &mut params);
            let sql = if only_present {
                format!(
                    "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT __k,{cols} FROM (SELECT {} AS __k,{} {from}{}) WHERE __k IN (SELECT __k FROM {all})",
                    rule.key,
                    rule.head.iter().enumerate().map(|(i, h)| format!("{h} AS c{i}")).collect::<Vec<_>>().join(","),
                    rule_where(rule, None)
                )
            } else {
                format!(
                    "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT {},{} {from}{}",
                    rule.key,
                    rule.head.join(","),
                    rule_where(rule, None)
                )
            };
            db.execute_cached(&sql, params_from_iter(params))
        };
        let rounds = |mut lo: i64| -> Result<()> {
            let mut rounds = 0usize;
            loop {
                if rounds >= BULK_ROUND_BUDGET {
                    return Err(error("fixpoint closure round budget exceeded"));
                }
                rounds += 1;
                let hi = max_rowid(db, &all)?;
                if hi == lo {
                    return Ok(());
                }
                let round = round_span("derive");
                let mut written = 0;
                for rule in rules.iter().filter(|r| r.member().is_some()) {
                    written += derive(
                        &all,
                        rule,
                        &roles(name, id, side, rule, None, Role::Range(all.clone(), lo, hi)),
                        false,
                    )?;
                }
                round.record("rows", written);
                lo = hi;
            }
        };
        let read = |sql: &str, params: &[Value], sign: i64| -> Result<Vec<Delta>> {
            Ok(rows(db, sql, params)?
                .into_iter()
                .map(|r| (r, sign))
                .collect())
        };
        if new > 0 {
            let lo = max_rowid(db, &all)?;
            for rule in rules.iter().filter(|r| r.mentions(side)) {
                derive(
                    &all,
                    rule,
                    &roles(name, id, side, rule, Some(row), Role::Table(all.clone())),
                    false,
                )?;
            }
            rounds(lo)?;
            return read(
                &format!("SELECT {cols} FROM {all} WHERE rowid>?1"),
                &[Value::Integer(lo)],
                1,
            );
        }
        db.execute_cached(&format!("DELETE FROM {work}"), [])?;
        for rule in rules.iter().filter(|r| r.mentions(side)) {
            derive(
                &work,
                rule,
                &roles(name, id, side, rule, Some(row), Role::Table(all.clone())),
                true,
            )?;
        }
        let mut lo = 0;
        let mut delete_rounds = 0usize;
        let mut deleted = BTreeMap::<String, Row>::new();
        loop {
            if delete_rounds >= BULK_ROUND_BUDGET {
                return Err(error("fixpoint closure round budget exceeded"));
            }
            delete_rounds += 1;
            let hi = max_rowid(db, &work)?;
            if hi == lo {
                break;
            }
            let round = round_span("delete");
            rows(
                db,
                &format!("SELECT __k,{cols} FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"),
                &[Value::Integer(lo), Value::Integer(hi)],
            )?
            .into_iter()
            .try_for_each(|mut stored| {
                let Value::Text(k) = stored.remove(0) else {
                    return Err(error("invalid fixpoint member key"));
                };
                deleted.entry(k).or_insert(stored);
                Ok(())
            })?;
            db.execute_cached(
                &format!("DELETE FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"),
                rusqlite::params![lo, hi],
            )?;
            let mut written = 0;
            for rule in rules.iter().filter(|r| r.member().is_some()) {
                written += derive(
                    &work,
                    rule,
                    &roles(
                        name,
                        id,
                        side,
                        rule,
                        None,
                        Role::Range(work.clone(), lo, hi),
                    ),
                    true,
                )?;
            }
            round.record("rows", written);
            lo = hi;
        }
        let mut params = vec![];
        let mut derivable = vec![];
        for rule in rules {
            let from = rule_from(
                rule,
                &roles(name, id, side, rule, None, Role::Table(all.clone())),
                &mut params,
            );
            let matched = rule
                .head
                .iter()
                .zip(&node.fields)
                .enumerate()
                .map(|(i, (h, f))| format!("(({h}) COLLATE {}) IS w.c{i}", f.collation))
                .collect::<Vec<_>>()
                .join(" AND ");
            derivable.push(format!(
                "EXISTS(SELECT 1 {from}{})",
                rule_where(rule, Some(&matched))
            ));
        }
        let restored = max_rowid(db, &all)?;
        db.execute_cached(
            &format!(
                "INSERT OR IGNORE INTO {all}(__k,{cols}) SELECT w.__k,{} FROM {work} w WHERE {}",
                (0..width)
                    .map(|i| format!("w.c{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                derivable.join(" OR ")
            ),
            params_from_iter(params),
        )?;
        rounds(restored)?;
        let removed = deleted.into_iter().try_fold(
            Vec::new(),
            |mut deltas, (k, stored)| -> Result<Vec<Delta>> {
                let current = rows(
                    db,
                    &format!("SELECT {cols} FROM {all} WHERE __k=?1"),
                    &[Value::Text(k)],
                )?
                .into_iter()
                .next();
                match current {
                    None => deltas.push((stored, -1)),
                    Some(current) if identity(db, &stored)? != identity(db, &current)? => {
                        deltas.push((stored, -1));
                        deltas.push((current, 1));
                    }
                    Some(_) => {}
                }
                Ok(deltas)
            },
        )?;
        db.execute_cached(&format!("DELETE FROM {work}"), [])?;
        Ok(removed)
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
                body.push_str(&format!(
                    "INSERT INTO {}(__ivm_source,__ivm_adding,{}) VALUES({source},{},{});",
                    quote(name),
                    (0..s.columns.len())
                        .map(|i| format!("__ivm_v{i}"))
                        .collect::<Vec<_>>()
                        .join(","),
                    adding,
                    s.columns
                        .iter()
                        .map(|c| format!("{image}.{}", quote(c)))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
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
