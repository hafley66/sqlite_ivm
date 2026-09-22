use crate::{
    catalog::{error, quote},
    statements::{self, Phase},
    relational::{Kind, Plan, Rule},
    relational_maintenance::{
        columns, out_table, roles, rule_from, rule_where, table,
        BULK_GROUP_BUDGET, BULK_MULTIPLICITY_BUDGET, BULK_ROUND_BUDGET, Role,
    },
};
use rusqlite::{params_from_iter, types::Value, Connection, Result};

impl Plan {
    pub(crate) fn materialize(&self, db: &Connection, name: &str, id: usize, restricted: bool) -> Result<()> {
        self.materialize_statements(db, name, id, restricted)
            .execute(db, name)
            .map(|_| ())
    }
    /// Every SQL string materializing this node issues, built once and reused by
    /// both the populate path and the per-view drain program.
    pub(crate) fn materialize_statements(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        restricted: bool,
    ) -> MaterializeStatements {
        self.materialize_from_sources(db, name, id, restricted, None)
    }
    pub(crate) fn materialize_from_sources(
        &self, db: &Connection, name: &str, id: usize, restricted: bool,
        sources: Option<&[String]>,
    ) -> MaterializeStatements {
        let node = &self.nodes[id];
        let out = out_table(id, node.fields.len());
        let cols = columns(node.fields.len());
        // source_table never fails: it formats one of two static shapes.
        let source = |side: usize| -> String {
            if let Some(sources) = sources { return sources[side].clone(); }
            let Ok(t) = self.source_table(db, name, id, side, restricted) else {
                unreachable!()
            };
            t
        };
        match &node.kind {
            Kind::Input(source_index) => {
                let s = &self.sources[*source_index];
                MaterializeStatements::Input {
                    insert: format!(
                        "INSERT INTO {out}({cols},__m) SELECT {},1 FROM main.{}",
                        s.columns
                            .iter()
                            .map(|c| quote(c))
                            .collect::<Vec<_>>()
                            .join(","),
                        quote(&s.name)
                    ),
                }
            }
            Kind::Map { expressions, predicate } => MaterializeStatements::Map {
                insert: format!(
                    "INSERT INTO {out}({cols},__m) SELECT {},__m FROM {child}{}",
                    expressions.join(","),
                    predicate
                        .as_ref()
                        .map(|p| format!(" WHERE {p}"))
                        .unwrap_or_default(),
                    child = out_table(node.inputs[0], self.nodes[node.inputs[0]].fields.len())
                ),
            },
            Kind::Set(op) => {
                let width = node.fields.len();
                let reps = |t: &str| {
                    format!(
                        "SELECT a.c0{},1 FROM (SELECT *,min(rowid) FROM {t} GROUP BY __k) a WHERE true",
                        (1..width).map(|i| format!(",a.c{i}")).collect::<String>()
                    )
                };
                let t0 = source(0);
                let t1 = if node.inputs.len() == 2 {
                    source(1)
                } else {
                    String::new()
                };
                let insert = match (*op, node.inputs.len()) {
                    ("all", _) => {
                        let child =
                            out_table(node.inputs[0], self.nodes[node.inputs[0]].fields.len());
                        format!("INSERT INTO {out}({cols},__m) SELECT {cols},__m FROM {child}")
                    }
                    ("distinct", _) | (_, 1) => {
                        format!("INSERT INTO {out}({cols},__m) {}", reps(&t0))
                    }
                    ("union", _) => format!(
                        "INSERT INTO {out}({cols},__m) {} UNION ALL SELECT a.c0{},1 FROM (SELECT *,min(rowid) FROM {} GROUP BY __k) a WHERE a.__k NOT IN(SELECT __k FROM {})",
                        reps(&t0),
                        (1..width).map(|i| format!(",a.c{i}")).collect::<String>(),
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
                MaterializeStatements::Set { insert }
            }
            Kind::Join { mode, predicate, .. } => {
                let left_n = self.nodes[node.inputs[0]].fields.len();
                let right_n = self.nodes[node.inputs[1]].fields.len();
                let t0 = source(0);
                let t1 = source(1);
                let matched = self.native_join_match(id);
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
                let restriction = predicate.as_deref().unwrap_or("1");
                let combined = format!(
                    "SELECT {},{},l.__n*r.__n AS __m FROM {t0} l JOIN {t1} r ON {matched}",
                    left_cols("l"),
                    right_cols("r")
                );
                let inner = match predicate {
                    Some(p) => format!("SELECT * FROM ({combined}) WHERE {p}"),
                    None => combined,
                };
                let unmatched_left = format!(
                    "SELECT {},{},l.__n AS __m FROM {t0} l WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE {matched}) m WHERE {})",
                    left_cols("l"),
                    null_cols(left_n, right_n),
                    left_pair("l", "r"),
                    restriction
                );
                let unmatched_right = format!(
                    "SELECT {},{},r.__n AS __m FROM {t1} r WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t0} l WHERE {matched}) m WHERE {})",
                    null_cols(0, left_n),
                    right_cols("r"),
                    right_pair("r", "l"),
                    restriction
                );
                let body = match *mode {
                    "inner" => inner,
                    "left" => format!("{inner} UNION ALL {unmatched_left}"),
                    "right" => format!("{inner} UNION ALL {unmatched_right}"),
                    "full" => format!("{inner} UNION ALL {unmatched_left} UNION ALL {unmatched_right}"),
                    "semi" => format!(
                        "SELECT {},l.__n AS __m FROM {t0} l WHERE EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE {matched}) m WHERE {})",
                        left_cols("l"),
                        left_pair("l", "r"),
                        restriction
                    ),
                    _ => format!(
                        "SELECT {},l.__n AS __m FROM {t0} l WHERE NOT EXISTS(SELECT 1 FROM (SELECT {} FROM {t1} r WHERE {matched}) m WHERE {})",
                        left_cols("l"),
                        left_pair("l", "r"),
                        restriction
                    ),
                };
                MaterializeStatements::Join {
                    insert: format!("INSERT INTO {out}({cols},__m) SELECT * FROM ({body})"),
                    bad: format!(
                        "SELECT EXISTS(SELECT 1 FROM {out} WHERE typeof(__m)!='integer' OR __m<0)"
                    ),
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
                let t = source(0);
                let replaced = expressions
                    .iter()
                    .map(|e| e.replace("__window__", &format!("ORDER BY {}", order.join(","))))
                    .collect::<Vec<_>>()
                    .join(",");
                let mut statements = GroupStatements {
                    window: *window,
                    limit: *limit,
                    offset: *offset,
                    peak: format!("SELECT coalesce(max(__n),0) FROM {t}"),
                    groups: format!("SELECT count(DISTINCT __k) FROM {t}"),
                    window_insert: String::new(),
                    limit_insert: String::new(),
                    limit_keys: format!("SELECT DISTINCT __k FROM {t}"),
                    plain_insert: {
                        let mut sql =
                            format!("INSERT INTO {out}({cols},__m) SELECT {replaced},1 FROM {t}");
                        if !keys.is_empty() {
                            sql.push_str(&format!(" GROUP BY {}", keys.join(",")));
                        }
                        if let Some(h) = having {
                            sql.push_str(&format!(" HAVING {h}"));
                        }
                        sql
                    },
                };
                if *window {
                    // Every partition in one statement: the window runs over the
                    // expanded bag partitioned by key, instead of once per key.
                    let width = self.nodes[node.inputs[0]].fields.len();
                    let cols_in = columns(width);
                    let partitioned = expressions
                        .iter()
                        .map(|e| e.replacen(" OVER(", " OVER(PARTITION BY __k ", 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    statements.window_insert = format!(
                        "WITH RECURSIVE candidates(__k,{cols_in},__n) AS (SELECT __k,{cols_in},__n FROM {t}),                              copies(__copy) AS (SELECT 1 UNION ALL SELECT __copy+1 FROM copies WHERE __copy<(SELECT coalesce(max(__n),0) FROM candidates))                              INSERT INTO {out}({cols},__m) SELECT q.*,1 FROM (SELECT {partitioned} FROM candidates CROSS JOIN copies WHERE __copy<=__n) q"
                    );
                }
                if limit.is_some() {
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
                    statements.limit_insert =
                        format!("INSERT INTO {out}({cols},__m) SELECT q.*,1 FROM ({single}) q");
                }
                MaterializeStatements::Group(statements)
            }
            Kind::Fixpoint { rules } => {
                let member = node.inputs.len();
                let all = table(name, id, member);
                let anchor = rules
                    .iter()
                    .map(|rule| {
                        let mut params: Vec<Value> = vec![];
                        let from = rule_from(
                            rule,
                            &roles(self, db, false, name, id, 0, rule, None, Role::Table(all.clone())),
                            &mut params,
                        );
                        fixpoint_derive(&all, &cols, rule, &from)
                    })
                    .collect();
                let rounds = rules
                    .iter()
                    .filter(|r| r.member().is_some())
                    .map(|rule| {
                        let mut params: Vec<Value> = vec![];
                        let from = rule_from(
                            rule,
                            &roles(self, db, false, name, id, 0, rule, None, Role::Range(all.clone(), 0, 0)),
                            &mut params,
                        );
                        fixpoint_derive(&all, &cols, rule, &from)
                    })
                    .collect();
                let copy = format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 FROM {all}");
                MaterializeStatements::Fixpoint(FixpointMaterializeStatements {
                    all,
                    anchor,
                    rounds,
                    copy,
                })
            }
        }
    }
}

/// The fixpoint member derive: the rowid range rides in `?1` and `?2` when the
/// rule's member occurrence scans one.
fn fixpoint_derive(target: &str, cols: &str, rule: &Rule, from: &str) -> String {
    format!(
        "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT {},{} {from}{}",
        rule.key,
        rule.head.join(","),
        rule_where(rule, None)
    )
}

pub(crate) enum MaterializeStatements {
    Input { insert: String },
    Map { insert: String },
    Set { insert: String },
    Join { insert: String, bad: String },
    Group(GroupStatements),
    Fixpoint(FixpointMaterializeStatements),
}

pub(crate) struct GroupStatements {
    pub(crate) window: bool,
    pub(crate) limit: Option<i64>,
    pub(crate) offset: i64,
    pub(crate) peak: String,
    pub(crate) groups: String,
    pub(crate) window_insert: String,
    pub(crate) limit_insert: String,
    pub(crate) limit_keys: String,
    pub(crate) plain_insert: String,
}

pub(crate) struct FixpointMaterializeStatements {
    pub(crate) all: String,
    pub(crate) anchor: Vec<String>,
    pub(crate) rounds: Vec<String>,
    pub(crate) copy: String,
}

impl MaterializeStatements {
    /// Executes this node and returns the number of rows written to its out
    /// table. The drain carries that count forward instead of probing the table.
    pub(crate) fn execute(&self, db: &Connection, name: &str) -> Result<usize> {
        let written = match self {
            MaterializeStatements::Input { insert }
            | MaterializeStatements::Map { insert }
            | MaterializeStatements::Set { insert } => {
                statements::exec_cached(db, Phase::Materialize, name, insert, [])?
            }
            MaterializeStatements::Join { insert, bad } => {
                let written = statements::exec_cached(db, Phase::Materialize, name, insert, [])?;
                let bad: bool =
                    statements::query_cached(db, Phase::Materialize, name, bad, [], |r| r.get(0))?;
                if bad {
                    return Err(error("join multiplicity overflow"));
                }
                written
            }
            MaterializeStatements::Group(g) => {
                if g.window || g.limit.is_some() {
                    let peak: i64 = statements::query_cached(db, Phase::Materialize, name, &g.peak, [], |r| {
                        r.get(0)
                    })?;
                    let expansion = match g.limit.filter(|n| *n >= 0) {
                        Some(n) if !g.window => peak.min(n.saturating_add(g.offset)),
                        _ => peak,
                    };
                    if expansion > BULK_MULTIPLICITY_BUDGET {
                        return Err(error("group multiplicity expansion exceeds budget"));
                    }
                }
                if g.window {
                    let groups: i64 = statements::query_cached(db, Phase::Materialize, name, &g.groups, [], |r| {
                        r.get(0)
                    })?;
                    if groups as usize > BULK_GROUP_BUDGET {
                        return Err(error("bulk group budget exceeded"));
                    }
                    statements::exec_cached(db, Phase::Materialize, name, &g.window_insert, [])?
                } else if g.limit.is_some() {
                    // One span per key insert, so each execution carries its own
                    // changes() count. The key read loop is bounded below.
                    let read = statements::open(
                        Phase::Materialize,
                        name,
                        &g.limit_keys,
                        statements::CACHED,
                    );
                    let _read = read.enter();
                    let mut key_statement = db.prepare_cached(&g.limit_keys)?;
                    let mut key_rows = key_statement.query([])?;
                    let mut groups = 0usize;
                    let mut written = 0usize;
                    while let Some(key) = key_rows.next()? {
                        groups += 1;
                        if groups > BULK_GROUP_BUDGET {
                            return Err(error("bulk group budget exceeded"));
                        }
                        written += statements::exec_cached(
                            db,
                            Phase::Materialize,
                            name,
                            &g.limit_insert,
                            [key.get::<_, Value>(0)?],
                        )?;
                    }
                    drop(key_rows);
                    read.rows(groups);
                    written
                } else {
                    statements::exec_cached(db, Phase::Materialize, name, &g.plain_insert, [])?
                }
            }
            MaterializeStatements::Fixpoint(f) => {
                let mut rounds = 0usize;
                let max_rowid = format!("SELECT coalesce(max(rowid),0) FROM {}", f.all);
                let mut lo =
                    statements::query_cached(db, Phase::Materialize, name, &max_rowid, [], |r| {
                        r.get(0)
                    })?;
                for sql in &f.anchor {
                    statements::exec_cached(db, Phase::Materialize, name, sql, [])?;
                }
                loop {
                    if rounds >= BULK_ROUND_BUDGET {
                        return Err(error("fixpoint closure round budget exceeded"));
                    }
                    rounds += 1;
                    let hi =
                        statements::query_cached(db, Phase::Materialize, name, &max_rowid, [], |r| {
                            r.get(0)
                        })?;
                    if hi == lo {
                        break;
                    }
                    let mut written = 0usize;
                    for sql in &f.rounds {
                        written += statements::exec_cached(
                            db,
                            Phase::Materialize,
                            name,
                            sql,
                            params_from_iter([Value::Integer(lo), Value::Integer(hi)]),
                        )?;
                    }
                    if written == 0 {
                        break;
                    }
                    lo = hi;
                }
                statements::exec(db, Phase::Materialize, name, &f.copy, [])?
            }
        };
        Ok(written)
    }
}
