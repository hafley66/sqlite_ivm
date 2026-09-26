//! SQLite engine: `lower` emits one delta table and one INSERT..SELECT per node; all state lives in tables.
//! Join uses the pre-image rule: every node integrates only after every fill of the settle has run.
//! Every read of integrated state is driven from a delta (CROSS JOIN fixes the loop order); aliases of
//! integrated tables end in `_i` so query plans name them.

use crate::ir::*;
use crate::rel::*;
use rusqlite::{params_from_iter, Connection};

const I64_MIN: &str = "(-9223372036854775807 - 1)";
const I64_MAX: &str = "9223372036854775807";

/// A node's SQL objects: `d` holds this settle's delta, `i` the integrated Z-set before this settle.
/// `rec` marks nodes that read a variable of the LetRec being lowered.
#[derive(Clone, Debug)]
pub struct SqlC {
    pub node: usize,
    pub d: String,
    pub i: String,
    pub arity: usize,
    pub rec: bool,
}

/// Who runs a node's fill and integration: the settle pass, or the round loop of one SCC.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Settle,
    Loop(usize),
    /// Filled by the settle pass, integrated by the SCC loop; an outer input frozen per round.
    Copy(usize),
}

struct Node {
    c: SqlC,
    fill: Vec<String>,
    /// Private state tables of the node (Reduce group accumulators, Min/Max arrangement).
    ddl: Vec<String>,
    integrated: bool,
    indexes: Vec<Vec<usize>>,
    owner: Owner,
}

/// One LetRec: per id the variable `v`, body counts `b`, and result `r` seen by later strata.
struct SccPlan {
    vars: Vec<(SqlC, SqlC, SqlC)>,
    at: usize,
}

struct Active {
    scc: usize,
    copies: Vec<(usize, SqlC)>,
}

pub struct SqlRel {
    nodes: Vec<Node>,
    sources: Vec<(RelId, SqlC)>,
    outputs: Vec<(RelId, SqlC)>,
    sccs: Vec<SccPlan>,
    active: Option<Active>,
    err: Option<EngineError>,
    /// Traced mode: `observe` records which SQL nodes each IR node built, and Join keeps its terms.
    traced: bool,
    /// SQL node count when the last observed node returned; nodes pushed after it belong to the next one.
    mark: usize,
    tags: Vec<(NodeId, bool, Vec<usize>)>,
    /// Join SQL node -> its three delta terms, each consolidated: Δa⋈Δb, I⁻a⋈Δb, Δa⋈I⁻b.
    terms: Vec<(usize, Vec<String>)>,
}

fn list(alias: &str, cols: impl IntoIterator<Item = usize>) -> String {
    let dot = if alias.is_empty() { String::new() } else { format!("{alias}.") };
    cols.into_iter().map(|c| format!("{dot}c{c}")).collect::<Vec<_>>().join(", ")
}

/// `a.cX = b.cY AND ...`, or `1` when there are no pairs.
fn on(a: &str, ac: &[usize], b: &str, bc: &[usize]) -> String {
    if ac.is_empty() {
        return "1".into();
    }
    ac.iter().zip(bc).map(|(x, y)| format!("{a}.c{x} = {b}.c{y}")).collect::<Vec<_>>().join(" AND ")
}

fn unsupported(what: &'static str) -> EngineError {
    EngineError::new(Stage::Install, None, ErrorKind::Unsupported(what))
}

fn sql_err(stage: Stage) -> impl Fn(rusqlite::Error) -> EngineError {
    move |e| EngineError::new(stage, None, ErrorKind::Worker(e.to_string()))
}

/// Expr as SQL text; comparisons and logic yield 0/1, Add/Sub wrap like `eval`.
fn render(e: &Expr, arity: usize, maps: &[String]) -> Result<String, EngineError> {
    Ok(match e {
        Expr::Col(c) if (*c as usize) < arity => format!("c{c}"),
        Expr::Col(c) => maps.get(*c as usize - arity).cloned().ok_or_else(|| unsupported("Mfp column out of range"))?,
        Expr::Lit(v) if *v == i64::MIN => I64_MIN.into(),
        Expr::Lit(v) => format!("({v})"),
        Expr::Call(func, args) => {
            let a = |i: usize| -> Result<String, EngineError> {
                let arg = args.get(i).ok_or_else(|| unsupported("Func arity"))?;
                render(arg, arity, maps)
            };
            let bin = |op: &str| -> Result<String, EngineError> { Ok(format!("(({}) {op} ({}))", a(0)?, a(1)?)) };
            match func {
                Func::Eq => bin("=")?,
                Func::Ne => bin("<>")?,
                Func::Lt => bin("<")?,
                Func::Le => bin("<=")?,
                Func::Gt => bin(">")?,
                Func::Ge => bin(">=")?,
                Func::Add => {
                    let (x, y) = (a(0)?, a(1)?);
                    format!(
                        "(CASE WHEN {y} > 0 AND {x} > {I64_MAX} - {y} THEN ({x} + {I64_MIN}) + ({y} + {I64_MIN}) \
                         WHEN {y} < 0 AND {x} < {I64_MIN} - {y} THEN ({x} - {I64_MIN}) + ({y} - {I64_MIN}) \
                         ELSE {x} + {y} END)"
                    )
                }
                Func::Sub => {
                    let (x, y) = (a(0)?, a(1)?);
                    format!(
                        "(CASE WHEN {y} < 0 AND {x} > {I64_MAX} + {y} THEN ({x} + {I64_MIN}) - ({y} - {I64_MIN}) \
                         WHEN {y} > 0 AND {x} < {I64_MIN} + {y} THEN ({x} - {I64_MIN}) - ({y} + {I64_MIN}) \
                         ELSE {x} - {y} END)"
                    )
                }
                Func::And => format!("((({}) <> 0) AND (({}) <> 0))", a(0)?, a(1)?),
                Func::Or => format!("((({}) <> 0) OR (({}) <> 0))", a(0)?, a(1)?),
                Func::Not => format!("(({}) = 0)", a(0)?),
            }
        }
    })
}

/// Distinct key values of `c`'s delta as `c0..`; one row or none for an empty key.
fn touched(c: &SqlC, key: &[usize]) -> String {
    if key.is_empty() {
        return format!("(SELECT 1 FROM {} LIMIT 1) t", c.d);
    }
    format!("(SELECT DISTINCT {} FROM {}) t", key.iter().enumerate().map(|(t, k)| format!("c{k} AS c{t}")).collect::<Vec<_>>().join(", "), c.d)
}

/// Rows of `table` whose `at` columns equal a key of `c`'s delta, read by index from the touched keys.
fn of_touched(c: &SqlC, key: &[usize], table: &str, at: &[usize], arity: usize, sign: &str) -> String {
    let t: Vec<usize> = (0..key.len()).collect();
    format!(
        "SELECT {}, {sign}x_i.w AS w FROM {} CROSS JOIN {table} x_i ON {}",
        (0..arity).map(|x| format!("x_i.c{x} AS c{x}")).collect::<Vec<_>>().join(", "),
        touched(c, key),
        on("t", &t, "x_i", at)
    )
}

impl SqlRel {
    fn new(p: &Program, traced: bool) -> Self {
        let mut rel = Self {
            nodes: Vec::new(),
            sources: Vec::new(),
            outputs: Vec::new(),
            sccs: Vec::new(),
            active: None,
            err: None,
            traced,
            mark: 0,
            tags: Vec::new(),
            terms: Vec::new(),
        };
        for r in p.rels.iter().filter(|r| r.kind == RelKind::Source) {
            let c = rel.push(r.cols.len(), false, |_| None);
            rel.integrate(&c, (0..c.arity).collect());
            rel.sources.push((r.id, c));
        }
        rel.mark = rel.nodes.len();
        rel
    }

    fn fail(&mut self, e: EngineError) {
        self.err.get_or_insert(e);
    }

    /// New node; `body` yields `c0.., w` rows and is wrapped in a consolidating GROUP BY.
    fn push(&mut self, arity: usize, rec: bool, body: impl FnOnce(&SqlC) -> Option<String>) -> SqlC {
        let node = self.nodes.len();
        let c = SqlC { node, d: format!("ivm_n{node}_d"), i: format!("ivm_n{node}_i"), arity, rec };
        if arity == 0 {
            self.fail(unsupported("zero-arity relation"));
        }
        let owner = match (&self.active, rec) {
            (Some(a), true) => Owner::Loop(a.scc),
            _ => Owner::Settle,
        };
        let fill = body(&c).into_iter().map(|body| {
            let cols = list("", 0..arity);
            format!(
                "INSERT INTO {d} ({cols}, w) SELECT {cols}, SUM(w) FROM ({body}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0",
                d = c.d
            )
        });
        let fill = fill.collect();
        self.nodes.push(Node { c: c.clone(), fill, ddl: Vec::new(), integrated: false, indexes: Vec::new(), owner });
        c
    }

    fn integrate(&mut self, c: &SqlC, index: Vec<usize>) {
        let node = &mut self.nodes[c.node];
        node.integrated = true;
        if !index.is_empty() && !node.indexes.contains(&index) {
            node.indexes.push(index);
        }
    }

    /// Outer input of a recursive node, replaced by an SCC-owned copy so the loop controls its delta.
    fn copy(&mut self, c: SqlC) -> SqlC {
        let Some(active) = &self.active else { return c };
        let scc = active.scc;
        if let Some((_, k)) = active.copies.iter().find(|(n, _)| *n == c.node) {
            return k.clone();
        }
        let k = self.push(c.arity, false, |_| Some(format!("SELECT {}, w FROM {}", list("", 0..c.arity), c.d)));
        self.nodes[k.node].owner = Owner::Copy(scc);
        self.integrate(&k, Vec::new());
        self.active.as_mut().unwrap().copies.push((c.node, k.clone()));
        k
    }

    fn feed(&mut self, cs: Vec<SqlC>) -> Vec<SqlC> {
        if !cs.iter().any(|c| c.rec) {
            return cs;
        }
        cs.into_iter().map(|c| if c.rec { c } else { self.copy(c) }).collect()
    }

    /// Non-monotone ops over a LetRec variable are outside the round loop's DRed contract.
    fn flat(&mut self, c: &SqlC, what: &'static str) {
        if c.rec {
            self.fail(unsupported(what));
        }
    }
}

impl Rel for SqlRel {
    type C = SqlC;

    fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError> {
        self.sources
            .iter()
            .find(|(id, _)| *id == rel)
            .map(|(_, c)| c.clone())
            .ok_or_else(|| EngineError::new(Stage::Install, Some(rel), ErrorKind::UnknownRel(rel)))
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C {
        let rendered = (|| -> Result<(Vec<String>, Vec<String>), EngineError> {
            let mut maps = Vec::new();
            for e in map {
                let m = render(e, c.arity, &maps)?;
                maps.push(m);
            }
            let wheres = filter.iter().map(|e| Ok(format!("({}) <> 0", render(e, c.arity, &[])?))).collect::<Result<_, _>>()?;
            let all: Vec<ColId> = (0..(c.arity + maps.len()) as ColId).collect();
            let proj = if project.is_empty() { &all[..] } else { project };
            let cols = proj.iter().map(|p| render(&Expr::Col(*p), c.arity, &maps)).collect::<Result<_, _>>()?;
            Ok((wheres, cols))
        })();
        let (wheres, cols) = rendered.unwrap_or_else(|e| {
            self.fail(e);
            (Vec::new(), vec!["0".into()])
        });
        self.push(cols.len(), c.rec, |_| {
            let select = cols.iter().enumerate().map(|(i, s)| format!("{s} AS c{i}")).collect::<Vec<_>>().join(", ");
            let wheres = if wheres.is_empty() { "1".into() } else { wheres.join(" AND ") };
            Some(format!("SELECT {select}, w FROM {} WHERE {wheres}", c.d))
        })
    }

    fn union(&mut self, cs: Vec<Self::C>) -> Self::C {
        let cs = self.feed(cs);
        let arity = cs.first().map_or(0, |c| c.arity);
        if cs.iter().any(|c| c.arity != arity) {
            self.fail(unsupported("Union arity mismatch"));
        }
        let cols = list("", 0..arity);
        let rec = cs.iter().any(|c| c.rec);
        self.push(arity, rec, |_| Some(cs.iter().map(|c| format!("SELECT {cols}, w FROM {}", c.d)).collect::<Vec<_>>().join(" UNION ALL ")))
    }

    fn negate(&mut self, c: Self::C) -> Self::C {
        self.flat(&c, "Negate over a LetRec variable");
        self.push(c.arity, false, |_| Some(format!("SELECT {}, -w AS w FROM {}", list("", 0..c.arity), c.d)))
    }

    /// Δ(a⋈b) = Δa⋈Δb + I⁻(a)⋈Δb + Δa⋈I⁻(b), with I⁻ the integrated tables not yet updated this settle.
    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>]) -> Result<Self::C, EngineError> {
        let [a, b]: [SqlC; 2] = self.feed(cs).try_into().map_err(|_| unsupported("Join arity != 2"))?;
        let side = |input: u8| -> Result<Vec<usize>, EngineError> {
            eq.iter()
                .map(|class| class.iter().find(|(i, _)| *i == input).map(|(_, c)| *c as usize))
                .collect::<Option<_>>()
                .ok_or_else(|| unsupported("Join class missing a side"))
        };
        let (lk, rk) = (side(0)?, side(1)?);
        self.integrate(&a, lk.clone());
        self.integrate(&b, rk.clone());
        let term = |l: &str, r: &str, l_first: bool| {
            let (la, ra) = (if l.ends_with("_i") { "l_i" } else { "l_d" }, if r.ends_with("_i") { "r_i" } else { "r_d" });
            let from = if l_first { format!("{l} {la} CROSS JOIN {r} {ra}") } else { format!("{r} {ra} CROSS JOIN {l} {la}") };
            format!(
                "SELECT {}, {}, {la}.w * {ra}.w AS w FROM {from} ON {}",
                (0..a.arity).map(|i| format!("{la}.c{i} AS c{i}")).collect::<Vec<_>>().join(", "),
                (0..b.arity).map(|i| format!("{ra}.c{i} AS c{}", a.arity + i)).collect::<Vec<_>>().join(", "),
                on(la, &lk, ra, &rk)
            )
        };
        let terms = [term(&a.d, &b.d, true), term(&a.i, &b.d, false), term(&a.d, &b.i, true)];
        let body = terms.join(" UNION ALL ");
        let out = self.push(a.arity + b.arity, a.rec || b.rec, |_| Some(body));
        if self.traced {
            let cols = list("", 0..out.arity);
            let terms = terms
                .iter()
                .map(|t| format!("SELECT {cols}, SUM(w) FROM ({t}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0 ORDER BY {cols}"))
                .collect();
            self.terms.push((out.node, terms));
        }
        Ok(out)
    }

    /// `l - l⋉threshold(π_rk r)`, built from this algebra's own join, threshold, negate and union.
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C {
        // With recursive `l`, an outer insertion into `r` removes derivations. The SCC loop
        // stashes positive outer deltas until after over-delete, so its rounds cannot seed that loss.
        self.flat(&l, "Antijoin over a LetRec variable");
        let unit = [r.arity as ColId];
        let keys = if rk.is_empty() { self.mfp(r, &[], &[Expr::Lit(0)], &unit) } else { self.mfp(r, &[], &[], rk) };
        let keys = self.threshold(keys);
        let eq: Vec<Vec<(u8, ColId)>> = lk.iter().enumerate().map(|(i, c)| vec![(0, *c), (1, i as ColId)]).collect();
        let joined = match self.join(vec![l.clone(), keys], &eq) {
            Ok(j) => j,
            Err(e) => {
                self.fail(e);
                return l;
            }
        };
        let left: Vec<ColId> = (0..l.arity as ColId).collect();
        let semi = self.mfp(joined, &[], &[], &left);
        let gone = self.negate(semi);
        self.union(vec![l, gone])
    }

    /// Per-group accumulators (count, sums) take Δinput by upsert; Min/Max read a private arrangement of
    /// the input by index. Touched groups emit their new row minus their stored row.
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg]) -> Self::C {
        self.flat(&c, "Reduce over a LetRec variable");
        let key: Vec<usize> = key.iter().map(|k| *k as usize).collect();
        let arity = key.len() + aggs.len();
        let out = self.push(arity, false, |_| None);
        let k = out.node;
        let (g, arr) = (format!("ivm_n{k}_g"), format!("ivm_n{k}_in"));
        let gk: Vec<usize> = (0..key.len().max(1)).collect();
        let gcols = gk.iter().map(|x| format!("g{x}")).collect::<Vec<_>>().join(", ");
        let kx = if key.is_empty() { vec!["0".to_string()] } else { key.iter().map(|x| format!("c{x}")).collect() };
        let group = if key.is_empty() { "HAVING COUNT(*) > 0".to_string() } else { format!("GROUP BY {}", kx.join(", ")) };
        let sums: Vec<(usize, ColId)> = aggs.iter().enumerate().filter_map(|(j, a)| if let Agg::Sum(x) = a { Some((j, *x)) } else { None }).collect();
        let extremes: Vec<ColId> = aggs.iter().filter_map(|a| if let Agg::Min(x) | Agg::Max(x) = a { Some(*x) } else { None }).collect();
        let s_cols: String = sums.iter().map(|(j, _)| format!(", a{j}")).collect();
        let mut ddl = vec![format!(
            "CREATE TABLE {g} ({}, cnt INTEGER NOT NULL{}, PRIMARY KEY ({gcols})) WITHOUT ROWID",
            gk.iter().map(|x| format!("g{x} INTEGER NOT NULL")).collect::<Vec<_>>().join(", "),
            sums.iter().map(|(j, _)| format!(", a{j} INTEGER NOT NULL")).collect::<String>()
        )];
        let mut fill = vec![format!(
            "INSERT INTO {g} ({gcols}, cnt{s_cols}) SELECT {}, SUM(w){} FROM {d} WHERE true {group} \
             ON CONFLICT ({gcols}) DO UPDATE SET cnt = cnt + excluded.cnt{}",
            kx.join(", "),
            sums.iter().map(|(_, x)| format!(", SUM(c{x} * w)")).collect::<String>(),
            sums.iter().map(|(j, _)| format!(", a{j} = a{j} + excluded.a{j}")).collect::<String>(),
            d = c.d
        )];
        let all = list("", 0..c.arity);
        if !extremes.is_empty() {
            ddl.push(format!("CREATE TABLE {arr} ({}, w INTEGER NOT NULL, PRIMARY KEY ({all})) WITHOUT ROWID", decl(c.arity)));
            for x in &extremes {
                let cols = key.iter().chain([&(*x as usize)]).map(|x| format!("c{x}")).collect::<Vec<_>>().join(", ");
                ddl.push(format!("CREATE INDEX IF NOT EXISTS ivm_n{k}_in{x} ON {arr} ({cols}, w)"));
            }
            fill.push(format!(
                "INSERT INTO {arr} ({all}, w) SELECT {all}, SUM(w) FROM {d} WHERE true GROUP BY {all} ON CONFLICT ({all}) DO UPDATE SET w = w + excluded.w",
                d = c.d
            ));
            fill.push(format!("DELETE FROM {arr} WHERE w = 0 AND ({all}) IN (SELECT {all} FROM {d})", d = c.d));
        }
        let g_on = |alias: &str| {
            let pairs: Vec<String> = key.iter().zip(&gk).map(|(x, t)| format!("n_i.c{x} = {alias}.g{t}")).collect();
            if pairs.is_empty() { "1".to_string() } else { pairs.join(" AND ") }
        };
        let values = aggs.iter().enumerate().map(|(j, agg)| match agg {
            Agg::Count => "g_i.cnt".to_string(),
            Agg::Sum(_) => format!("g_i.a{j}"),
            Agg::Min(x) => format!("(SELECT n_i.c{x} FROM {arr} n_i WHERE {} AND n_i.w > 0 ORDER BY n_i.c{x} LIMIT 1)", g_on("g_i")),
            Agg::Max(x) => format!("(SELECT n_i.c{x} FROM {arr} n_i WHERE {} AND n_i.w > 0 ORDER BY n_i.c{x} DESC LIMIT 1)", g_on("g_i")),
        });
        let select = gk.iter().take(key.len()).map(|x| format!("g_i.g{x}")).chain(values).enumerate().map(|(i, s)| format!("{s} AS c{i}")).collect::<Vec<_>>().join(", ");
        let body = format!(
            "SELECT {select}, 1 AS w FROM (SELECT DISTINCT {kg} FROM {d}) t CROSS JOIN {g} g_i ON {t_on} WHERE g_i.cnt > 0 UNION ALL {gone}",
            kg = kx.iter().zip(&gk).map(|(e, x)| format!("{e} AS g{x}")).collect::<Vec<_>>().join(", "),
            d = c.d,
            t_on = gk.iter().map(|x| format!("t.g{x} = g_i.g{x}")).collect::<Vec<_>>().join(" AND "),
            gone = of_touched(&c, &key, &out.i, &(0..key.len()).collect::<Vec<_>>(), arity, "-"),
        );
        let cols = list("", 0..arity);
        fill.push(format!("INSERT INTO {} ({cols}, w) SELECT {cols}, SUM(w) FROM ({body}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0", out.d));
        fill.push(format!(
            "DELETE FROM {g} WHERE cnt = 0{} AND ({gcols}) IN (SELECT {} FROM {d})",
            sums.iter().map(|(j, _)| format!(" AND a{j} = 0")).collect::<String>(),
            kx.join(", "),
            d = c.d
        ));
        let node = &mut self.nodes[k];
        node.fill = fill;
        node.ddl = ddl;
        self.integrate(&out, Vec::new());
        out
    }

    /// Emits ±1 only where the accumulated input weight crosses zero.
    fn threshold(&mut self, c: Self::C) -> Self::C {
        self.flat(&c, "Threshold over a LetRec variable");
        self.integrate(&c, Vec::new());
        let n: Vec<usize> = (0..c.arity).collect();
        self.push(c.arity, false, |_| {
            Some(format!(
                "SELECT {dc}, CASE WHEN COALESCE(i_i.w, 0) + d.w > 0 THEN 1 ELSE -1 END AS w \
                 FROM (SELECT {all}, SUM(w) AS w FROM {d} GROUP BY {all}) d LEFT JOIN {i} i_i ON {cond} \
                 WHERE (COALESCE(i_i.w, 0) > 0) <> (COALESCE(i_i.w, 0) + d.w > 0)",
                dc = n.iter().map(|x| format!("d.c{x} AS c{x}")).collect::<Vec<_>>().join(", "),
                all = list("", 0..c.arity),
                d = c.d,
                i = c.i,
                cond = on("i_i", &n, "d", &n),
            ))
        })
    }

    /// Touched groups re-ranked by `order` then the whole row ascending (DD `rank`); weight = rows taken.
    fn topk(&mut self, c: Self::C, key: &[ColId], order: &[Order], limit: u32) -> Result<Self::C, EngineError> {
        if c.rec {
            return Err(unsupported("TopK over a LetRec variable"));
        }
        let key: Vec<usize> = key.iter().map(|k| *k as usize).collect();
        self.integrate(&c, key.clone());
        let out = self.push(c.arity, false, |out| {
            let all = list("", 0..c.arity);
            let part = if key.is_empty() { String::new() } else { format!("PARTITION BY {}", list("", key.iter().copied())) };
            let by = order
                .iter()
                .map(|o| format!("c{}{}", o.col, if o.desc { " DESC" } else { "" }))
                .chain((0..c.arity).map(|x| format!("c{x}")))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!(
                "WITH cur AS (SELECT {all}, SUM(w) AS w FROM ({old} UNION ALL SELECT {all}, w FROM {d}) GROUP BY {all} HAVING SUM(w) > 0), \
                 ranked AS (SELECT {all}, w, SUM(w) OVER ({part} ORDER BY {by} ROWS UNBOUNDED PRECEDING) - w AS before FROM cur) \
                 SELECT {all}, MIN(w, {limit} - before) AS w FROM ranked WHERE before < {limit} \
                 UNION ALL {gone}",
                old = of_touched(&c, &key, &c.i, &key, c.arity, ""),
                d = c.d,
                gone = of_touched(&c, &key, &out.i, &key, c.arity, "-"),
            ))
        });
        self.integrate(&out, key);
        Ok(out)
    }

    /// Variables are SCC-owned tables maintained by a DRed round loop at settle; see `Sql::fixpoint`.
    fn letrec(&mut self, p: &Program, rec: &LetRec, defined: &[(RelId, Self::C)]) -> Result<Vec<Self::C>, EngineError> {
        if self.active.is_some() {
            return Err(unsupported("LetRec nested in LetRec"));
        }
        if rec.limit.is_some() {
            return Err(unsupported("LetRec limit"));
        }
        let scc = self.sccs.len();
        self.active = Some(Active { scc, copies: Vec::new() });
        let mut scope: Vec<(RelId, SqlC)> = defined.to_vec();
        let mut vs = Vec::new();
        for id in &rec.ids {
            let rel = p.rel(*id).ok_or_else(|| EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id)))?;
            let v = self.push(rel.cols.len(), true, |_| None);
            self.integrate(&v, Vec::new());
            scope.push((*id, v.clone()));
            vs.push(v);
        }
        self.mark = self.nodes.len();
        let mut nodes = vec![None; p.nodes.len()];
        let mut bs = Vec::new();
        for body in &rec.bodies {
            let c = lower_node(p, self, &mut nodes, &scope, *body)?;
            let b = if c.rec { c } else { self.copy(c) };
            self.integrate(&b, Vec::new());
            bs.push(b);
        }
        if vs.len() != bs.len() || vs.iter().zip(&bs).any(|(v, b)| v.arity != b.arity) {
            return Err(unsupported("LetRec ids and bodies differ in count or arity"));
        }
        self.active = None;
        let at = self.nodes.len();
        let mut vars = Vec::new();
        for (v, b) in vs.into_iter().zip(bs) {
            let r = self.push(v.arity, false, |_| None);
            vars.push((v, b, r.clone()));
        }
        let rs = vars.iter().map(|(_, _, r)| r.clone()).collect();
        self.sccs.push(SccPlan { vars, at });
        self.mark = self.nodes.len();
        Ok(rs)
    }

    fn output(&mut self, rel: RelId, c: Self::C) {
        self.integrate(&c, Vec::new());
        self.outputs.push((rel, c));
    }

    /// The returned SQL node is the IR node's own delta; SQL nodes pushed since the previous observe are its internals.
    fn observe(&mut self, id: NodeId, c: Self::C) -> Self::C {
        if !self.traced {
            return c;
        }
        let mut sql = vec![c.node];
        sql.extend((self.mark..self.nodes.len()).filter(|n| *n != c.node));
        self.mark = self.nodes.len();
        self.tags.push((id, self.active.is_some(), sql));
        c
    }
}

/// One observation of an IR node by the traced SQLite engine. An IR node lowered both outside and
/// inside a LetRec has one tag per lowering.
#[derive(Clone, Debug)]
pub struct SqlTag {
    pub id: NodeId,
    /// Lowered inside a LetRec; its tables may be cleared by the round loop before the trace reads them.
    pub in_loop: bool,
    /// Own delta table first, then internal ones.
    pub tables: Vec<String>,
    /// Fill statements per table, same order; a node with several fills has them joined by `;\n`.
    pub fills: Vec<String>,
    delta: String,
    totals: Option<String>,
    terms: Vec<String>,
}

/// What one tag's tables held after the fills of a settle and before integration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlSeen {
    pub id: NodeId,
    pub in_loop: bool,
    pub changes: Vec<(Row, W)>,
    /// The own `_i` table before this settle integrates; empty when the node keeps none.
    pub totals_before: Vec<(Row, W)>,
    /// Join only: Δa⋈Δb, I⁻a⋈Δb, Δa⋈I⁻b, each consolidated.
    pub terms: Vec<Vec<(Row, W)>>,
}

struct Source {
    rel: RelId,
    name: String,
    arity: usize,
    count: String,
    stage: String,
}

struct Output {
    rel: RelId,
    arity: usize,
    delta: String,
    snapshot: String,
}

enum Step {
    Fill(String),
    Loop(usize),
}

/// Statements of one variable's DRed phases; `nx` stages the next round's delta.
struct VarSql {
    over_delete: String,
    rederive: String,
    insert: String,
    to_delta: String,
    to_acc: String,
    to_del: String,
    clear_nx: String,
    clear_del: String,
    any: String,
    finish: String,
    clear_acc: String,
}

#[derive(Default)]
struct SccSql {
    fills: Vec<String>,
    integrates: Vec<String>,
    clears: Vec<String>,
    vars: Vec<VarSql>,
    /// Per copy: move positive rows aside, then later move them back.
    stash: Vec<String>,
    restore: Vec<String>,
}

pub struct Sql {
    pub conn: Connection,
    tick: u64,
    sources: Vec<Source>,
    steps: Vec<Step>,
    sccs: Vec<SccSql>,
    integrates: Vec<String>,
    clears: Vec<String>,
    outputs: Vec<Output>,
    /// Empty unless installed with `install_traced`.
    pub tags: Vec<SqlTag>,
}

fn read(conn: &Connection, sql: &str, arity: usize) -> rusqlite::Result<Vec<(Row, W)>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([], |r| Ok(((0..arity).map(|i| r.get(i)).collect::<Result<Row, _>>()?, r.get(arity)?)))?;
    rows.collect()
}

fn decl(arity: usize) -> String {
    (0..arity).map(|x| format!("c{x} INTEGER NOT NULL")).collect::<Vec<_>>().join(", ")
}

impl Sql {
    pub fn install(program: &Program) -> Result<Self, EngineError> {
        let conn = Connection::open_in_memory().map_err(sql_err(Stage::Install))?;
        Self::install_on(conn, program)
    }

    /// Runs `hook` on the fresh connection before any DDL; the engine keeps no counters itself.
    pub fn install_observed(program: &Program, hook: Box<dyn FnOnce(&Connection)>) -> Result<Self, EngineError> {
        let conn = Connection::open_in_memory().map_err(sql_err(Stage::Install))?;
        hook(&conn);
        Self::install_on(conn, program)
    }

    pub fn install_on(conn: Connection, p: &Program) -> Result<Self, EngineError> {
        Self::install_with(conn, p, false)
    }

    /// Tags every IR node with its SQL nodes; `settle_traced` reads their tables before integration.
    pub fn install_traced(program: &Program) -> Result<Self, EngineError> {
        let conn = Connection::open_in_memory().map_err(sql_err(Stage::Install))?;
        Self::install_with(conn, program, true)
    }

    /// `settle` plus one `SqlSeen` per tag; empty unless installed with `install_traced`.
    pub fn settle_traced(&mut self, frontier: Frontier) -> Result<(Delta, Vec<SqlSeen>), EngineError> {
        let settle = sql_err(Stage::Settle);
        self.conn.execute_batch("BEGIN").map_err(&settle)?;
        match self.run(&frontier) {
            Ok((changes, seen)) => {
                self.conn.execute_batch("COMMIT").map_err(&settle)?;
                self.tick += 1;
                Ok((Delta { tick: self.tick - 1, changes }, seen))
            }
            Err(e) => {
                self.conn.execute_batch("ROLLBACK").map_err(&settle)?;
                Err(e)
            }
        }
    }

    fn install_with(conn: Connection, p: &Program, traced: bool) -> Result<Self, EngineError> {
        let mut rel = SqlRel::new(p, traced);
        lower(p, &mut rel)?;
        if let Some(e) = rel.err {
            return Err(e);
        }
        let mut ddl = Vec::new();
        let (mut steps, mut integrates, mut clears) = (Vec::new(), Vec::new(), Vec::new());
        let mut sccs: Vec<SccSql> = rel.sccs.iter().map(|_| SccSql::default()).collect();
        for node in &rel.nodes {
            let SqlC { node: k, d, i, arity, .. } = &node.c;
            let cols = list("", 0..*arity);
            ddl.push(format!("CREATE TABLE {d} ({}, w INTEGER NOT NULL)", decl(*arity)));
            ddl.extend(node.ddl.iter().cloned());
            for (s, plan) in rel.sccs.iter().enumerate() {
                if plan.at == *k {
                    steps.push(Step::Loop(s));
                }
            }
            match node.owner {
                Owner::Loop(s) => sccs[s].fills.extend(node.fill.iter().cloned()),
                _ => steps.extend(node.fill.iter().cloned().map(Step::Fill)),
            }
            if let Owner::Copy(s) = node.owner {
                let hold = format!("ivm_n{k}_hold");
                ddl.push(format!("CREATE TABLE {hold} ({}, w INTEGER NOT NULL)", decl(*arity)));
                sccs[s].stash.extend([format!("INSERT INTO {hold} SELECT * FROM {d} WHERE w > 0"), format!("DELETE FROM {d} WHERE w > 0")]);
                sccs[s].restore.extend([format!("INSERT INTO {d} SELECT * FROM {hold}"), format!("DELETE FROM {hold}")]);
            }
            let (ints, clrs) = match node.owner {
                Owner::Settle => (&mut integrates, &mut clears),
                Owner::Loop(s) | Owner::Copy(s) => {
                    let scc = &mut sccs[s];
                    (&mut scc.integrates, &mut scc.clears)
                }
            };
            clrs.push(format!("DELETE FROM {d}"));
            if node.integrated {
                ddl.push(format!("CREATE TABLE {i} ({}, w INTEGER NOT NULL, PRIMARY KEY ({cols})) WITHOUT ROWID", decl(*arity)));
                for (j, index) in node.indexes.iter().enumerate() {
                    ddl.push(format!("CREATE INDEX ivm_n{k}_x{j} ON {i} ({})", list("", index.iter().copied())));
                }
                ints.push(format!(
                    "INSERT INTO {i} ({cols}, w) SELECT {cols}, SUM(w) FROM {d} WHERE true GROUP BY {cols} \
                     ON CONFLICT ({cols}) DO UPDATE SET w = w + excluded.w"
                ));
                ints.push(format!("DELETE FROM {i} WHERE w = 0 AND ({cols}) IN (SELECT {cols} FROM {d})"));
            }
        }
        for (plan, scc) in rel.sccs.iter().zip(&mut sccs) {
            for (v, b, r) in &plan.vars {
                let n = v.arity;
                let (cols, all) = (list("", 0..n), (0..n).collect::<Vec<_>>());
                let (nx, del, acc) = (format!("ivm_n{}_nx", v.node), format!("ivm_n{}_del", v.node), format!("ivm_n{}_acc", r.node));
                for t in [&nx, &del, &acc] {
                    ddl.push(format!("CREATE TABLE {t} ({}, w INTEGER NOT NULL)", decl(n)));
                }
                let live = |alias: &str, table: &str| format!("EXISTS (SELECT 1 FROM {table} x_i WHERE {} AND x_i.w > 0)", on("x_i", &all, alias, &all));
                scc.vars.push(VarSql {
                    over_delete: format!(
                        "INSERT INTO {nx} SELECT {cols}, -1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w < 0) t WHERE {}",
                        live("t", &v.i),
                        bd = b.d
                    ),
                    rederive: format!("INSERT INTO {nx} SELECT {cols}, 1 FROM {del} t WHERE {}", live("t", &b.i)),
                    insert: format!(
                        "INSERT INTO {nx} SELECT {cols}, 1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w > 0) t WHERE {} AND NOT {}",
                        live("t", &b.i),
                        live("t", &v.i),
                        bd = b.d
                    ),
                    to_delta: format!("INSERT INTO {} SELECT * FROM {nx}", v.d),
                    to_acc: format!("INSERT INTO {acc} SELECT * FROM {nx}"),
                    to_del: format!("INSERT INTO {del} SELECT * FROM {nx}"),
                    clear_nx: format!("DELETE FROM {nx}"),
                    clear_del: format!("DELETE FROM {del}"),
                    any: format!("SELECT EXISTS (SELECT 1 FROM {})", v.d),
                    finish: format!("INSERT INTO {} SELECT {cols}, SUM(w) FROM {acc} GROUP BY {cols} HAVING SUM(w) <> 0", r.d),
                    clear_acc: format!("DELETE FROM {acc}"),
                });
            }
        }
        let sources: Vec<Source> = rel
            .sources
            .iter()
            .map(|(id, c)| {
                let here = (0..c.arity).map(|x| format!("c{x} = ?{}", x + 1)).collect::<Vec<_>>().join(" AND ");
                ddl.push(format!("CREATE INDEX ivm_n{}_dx ON {} ({})", c.node, c.d, list("", 0..c.arity)));
                Source {
                    rel: *id,
                    name: p.rel(*id).map(|r| r.name.clone()).unwrap_or_default(),
                    arity: c.arity,
                    count: format!(
                        "SELECT COALESCE((SELECT w FROM {i} WHERE {here}), 0) + (SELECT COALESCE(SUM(w), 0) FROM {d} WHERE {here})",
                        i = c.i,
                        d = c.d
                    ),
                    stage: format!("INSERT INTO {} VALUES ({})", c.d, (1..=c.arity + 1).map(|x| format!("?{x}")).collect::<Vec<_>>().join(", ")),
                }
            })
            .collect();
        let mut outs = rel.outputs;
        outs.sort_by_key(|(id, _)| *id);
        let outputs: Vec<Output> = outs
            .iter()
            .map(|(id, c)| {
                let cols = list("", 0..c.arity);
                Output {
                    rel: *id,
                    arity: c.arity,
                    delta: format!("SELECT {cols}, SUM(w) FROM {} GROUP BY {cols} HAVING SUM(w) <> 0 ORDER BY {cols}", c.d),
                    snapshot: format!("SELECT {cols}, w FROM {} ORDER BY {cols}", c.i),
                }
            })
            .collect();
        let tags = rel
            .tags
            .iter()
            .map(|(id, in_loop, sql)| {
                let own = &rel.nodes[sql[0]];
                let cols = list("", 0..own.c.arity);
                SqlTag {
                    id: *id,
                    in_loop: *in_loop,
                    tables: sql.iter().map(|n| rel.nodes[*n].c.d.clone()).collect(),
                    fills: sql.iter().map(|n| rel.nodes[*n].fill.join(";\n")).collect(),
                    delta: format!("SELECT {cols}, SUM(w) FROM {} GROUP BY {cols} HAVING SUM(w) <> 0 ORDER BY {cols}", own.c.d),
                    totals: own.integrated.then(|| format!("SELECT {cols}, w FROM {} WHERE w <> 0 ORDER BY {cols}", own.c.i)),
                    terms: rel.terms.iter().find(|(n, _)| *n == sql[0]).map(|(_, t)| t.clone()).unwrap_or_default(),
                }
            })
            .collect();
        conn.execute_batch(&ddl.join(";\n")).map_err(sql_err(Stage::Install))?;
        let engine = Self { conn, tick: 0, sources, steps, sccs, integrates, clears, outputs, tags };
        let mut all = engine.statements();
        all.extend(engine.outputs.iter().map(|o| &o.snapshot));
        engine.conn.set_prepared_statement_cache_capacity(all.len() + 8);
        for sql in all {
            engine.conn.prepare_cached(sql).map_err(sql_err(Stage::Install))?;
        }
        Ok(engine)
    }

    /// Every statement settle can run; snapshot reads are left out.
    pub fn statements(&self) -> Vec<&String> {
        let mut all: Vec<&String> = self.sources.iter().flat_map(|s| [&s.count, &s.stage]).collect();
        all.extend(self.steps.iter().filter_map(|s| match s {
            Step::Fill(sql) => Some(sql),
            Step::Loop(_) => None,
        }));
        for scc in &self.sccs {
            all.extend(scc.fills.iter().chain(&scc.integrates).chain(&scc.clears).chain(&scc.stash).chain(&scc.restore));
            for v in &scc.vars {
                all.extend([
                    &v.over_delete, &v.rederive, &v.insert, &v.to_delta, &v.to_acc, &v.to_del, &v.clear_nx, &v.clear_del, &v.any, &v.finish,
                    &v.clear_acc,
                ]);
            }
        }
        all.extend(self.integrates.iter().chain(&self.clears));
        all.extend(self.outputs.iter().map(|o| &o.delta));
        all
    }

    fn exec(&self, sql: &str) -> Result<usize, EngineError> {
        self.conn.prepare_cached(sql).and_then(|mut s| s.execute([])).map_err(sql_err(Stage::Settle))
    }

    fn exec_all<'a>(&self, sqls: impl IntoIterator<Item = &'a String>) -> Result<(), EngineError> {
        sqls.into_iter().try_for_each(|sql| self.exec(sql).map(drop))
    }

    /// Stages accepted changes into source delta tables; same rules and order as the DD guard.
    fn guard(&self, frontier: &Frontier) -> Result<(), EngineError> {
        let settle = sql_err(Stage::Settle);
        for change in &frontier.changes {
            let src = self
                .sources
                .iter()
                .find(|s| s.rel == change.rel)
                .ok_or_else(|| EngineError::new(Stage::Settle, Some(change.rel), ErrorKind::UnknownRel(change.rel)))?;
            if change.row.len() != src.arity {
                let kind = ErrorKind::Arity { expected: src.arity, actual: change.row.len() };
                return Err(EngineError::new(Stage::Settle, Some(src.rel), kind));
            }
            if change.w != 1 && change.w != -1 {
                return Err(EngineError::new(Stage::Settle, Some(src.rel), ErrorKind::Unsupported("weight other than +1/-1")));
            }
            let before: W = self
                .conn
                .prepare_cached(&src.count)
                .and_then(|mut s| s.query_row(params_from_iter(&change.row), |r| r.get(0)))
                .map_err(&settle)?;
            if change.w > 0 && before > 0 {
                return Err(EngineError::new(Stage::Settle, Some(src.rel), ErrorKind::PresentInsert(change.row.clone())));
            }
            if change.w < 0 && before <= 0 {
                tracing::warn!(tick = self.tick, relation = %src.name, row = ?change.row, "delete of absent row ignored");
                continue;
            }
            self.conn
                .prepare_cached(&src.stage)
                .and_then(|mut s| s.execute(params_from_iter(change.row.iter().chain([&change.w]))))
                .map_err(&settle)?;
        }
        Ok(())
    }

    /// One pass of the SCC's node SQL, then each variable's next delta; returns whether any is non-empty.
    fn round(&self, scc: &SccSql, deleting: bool) -> Result<bool, EngineError> {
        self.exec_all(scc.fills.iter().chain(&scc.integrates))?;
        self.exec_all(scc.vars.iter().map(|v| if deleting { &v.over_delete } else { &v.insert }))?;
        self.exec_all(&scc.clears)?;
        let mut more = false;
        for v in &scc.vars {
            self.exec_all([&v.to_delta, &v.to_acc])?;
            if deleting {
                self.exec(&v.to_del)?;
            }
            self.exec(&v.clear_nx)?;
            more |= self.conn.prepare_cached(&v.any).and_then(|mut s| s.query_row([], |r| r.get::<_, bool>(0))).map_err(sql_err(Stage::Settle))?;
        }
        Ok(more)
    }

    /// DRed: over-delete every row that lost a derivation (outer deletions only), rederive
    /// deleted rows still supported, then propagate outer insertions and rederived rows.
    fn fixpoint(&self, scc: &SccSql) -> Result<(), EngineError> {
        self.exec_all(&scc.stash)?;
        while self.round(scc, true)? {}
        for v in &scc.vars {
            self.exec_all([&v.rederive, &v.clear_del, &v.to_delta, &v.to_acc, &v.clear_nx])?;
        }
        self.exec_all(&scc.restore)?;
        while self.round(scc, false)? {}
        for v in &scc.vars {
            self.exec_all([&v.finish, &v.clear_acc])?;
        }
        Ok(())
    }

    fn run(&self, frontier: &Frontier) -> Result<(Vec<(RelId, Row, W)>, Vec<SqlSeen>), EngineError> {
        self.guard(frontier)?;
        for step in &self.steps {
            match step {
                Step::Fill(sql) => self.exec(sql).map(drop)?,
                Step::Loop(s) => self.fixpoint(&self.sccs[*s])?,
            }
        }
        let mut changes = Vec::new();
        for out in &self.outputs {
            let rows = read(&self.conn, &out.delta, out.arity).map_err(sql_err(Stage::Settle))?;
            changes.extend(rows.into_iter().map(|(row, w)| (out.rel, row, w)));
        }
        let seen = self.seen().map_err(sql_err(Stage::Settle))?;
        self.exec_all(self.integrates.iter().chain(&self.clears))?;
        Ok((changes, seen))
    }

    /// Trace reads use uncached statements so the engine's statement cache is untouched.
    fn seen(&self) -> rusqlite::Result<Vec<SqlSeen>> {
        let read_once = |sql: &str| -> rusqlite::Result<Vec<(Row, W)>> {
            let mut stmt = self.conn.prepare(sql)?;
            let arity = stmt.column_count() - 1;
            let rows = stmt.query_map([], |r| Ok(((0..arity).map(|i| r.get(i)).collect::<Result<Row, _>>()?, r.get(arity)?)))?;
            rows.collect()
        };
        self.tags
            .iter()
            .map(|tag| {
                Ok(SqlSeen {
                    id: tag.id,
                    in_loop: tag.in_loop,
                    changes: read_once(&tag.delta)?,
                    totals_before: tag.totals.as_deref().map(read_once).transpose()?.unwrap_or_default(),
                    terms: tag.terms.iter().map(|t| read_once(t)).collect::<Result<_, _>>()?,
                })
            })
            .collect()
    }
}

impl Engine for Sql {
    fn install(program: &Program, _host: &mut impl Host) -> Result<Self, EngineError> {
        Self::install(program)
    }

    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        self.settle_traced(frontier).map(|(delta, _)| delta)
    }

    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        let out = self
            .outputs
            .iter()
            .find(|o| o.rel == rel)
            .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)))?;
        read(&self.conn, &out.snapshot, out.arity).map_err(sql_err(Stage::Snapshot))
    }
}
