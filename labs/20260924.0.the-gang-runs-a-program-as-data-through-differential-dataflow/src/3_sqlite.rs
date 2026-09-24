//! SQLite engine: `lower` emits one delta table and one INSERT..SELECT per node; all state lives in tables.
//! Join uses the pre-image rule: every node integrates only after every fill of the settle has run.

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
    fill: Option<String>,
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

/// Key values present in `c`'s delta, as `touched(c0..)`; one row or none for an empty key.
fn touched(c: &SqlC, key: &[usize]) -> String {
    if key.is_empty() {
        return format!("SELECT 1 FROM {} LIMIT 1", c.d);
    }
    format!("SELECT DISTINCT {} FROM {}", key.iter().enumerate().map(|(t, k)| format!("c{k} AS c{t}")).collect::<Vec<_>>().join(", "), c.d)
}

/// `EXISTS` test of `alias`'s `cols` against `touched`.
fn hit(alias: &str, cols: &[usize]) -> String {
    let t: Vec<usize> = (0..cols.len()).collect();
    format!("EXISTS (SELECT 1 FROM touched t WHERE {})", on("t", &t, alias, cols))
}

/// Current live rows of touched groups: I⁻(c) restricted to touched keys plus Δc, consolidated.
fn current(c: &SqlC, key: &[usize], having: &str) -> String {
    let all = list("", 0..c.arity);
    format!(
        "SELECT {all}, SUM(w) AS w FROM (SELECT {all}, w FROM {i} s WHERE {hit} UNION ALL SELECT {all}, w FROM {d}) \
         GROUP BY {all} HAVING SUM(w) {having}",
        i = c.i,
        d = c.d,
        hit = hit("s", key)
    )
}

impl SqlRel {
    fn new(p: &Program) -> Self {
        let mut rel = Self { nodes: Vec::new(), sources: Vec::new(), outputs: Vec::new(), sccs: Vec::new(), active: None, err: None };
        for r in p.rels.iter().filter(|r| r.kind == RelKind::Source) {
            let c = rel.push(r.cols.len(), false, |_| None);
            rel.integrate(&c, (0..c.arity).collect());
            rel.sources.push((r.id, c));
        }
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
        let fill = body(&c).map(|body| {
            let cols = list("", 0..arity);
            format!(
                "INSERT INTO {d} ({cols}, w) SELECT {cols}, SUM(w) FROM ({body}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0",
                d = c.d
            )
        });
        self.nodes.push(Node { c: c.clone(), fill, integrated: false, indexes: Vec::new(), owner });
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
        let select = format!(
            "SELECT {}, {}, l.w * r.w AS w",
            (0..a.arity).map(|i| format!("l.c{i} AS c{i}")).collect::<Vec<_>>().join(", "),
            (0..b.arity).map(|i| format!("r.c{i} AS c{}", a.arity + i)).collect::<Vec<_>>().join(", ")
        );
        let cond = on("l", &lk, "r", &rk);
        let terms = [(&a.d, &b.d), (&a.i, &b.d), (&a.d, &b.i)];
        let body = terms.iter().map(|(l, r)| format!("{select} FROM {l} l JOIN {r} r ON {cond}")).collect::<Vec<_>>().join(" UNION ALL ");
        Ok(self.push(a.arity + b.arity, a.rec || b.rec, |_| Some(body)))
    }

    /// `l - l⋉threshold(π_rk r)`, built from this algebra's own join, threshold, negate and union.
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C {
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

    /// Recomputes touched groups from I⁻(input) + Δinput, diffed against the stored group rows.
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg]) -> Self::C {
        self.flat(&c, "Reduce over a LetRec variable");
        let key: Vec<usize> = key.iter().map(|k| *k as usize).collect();
        self.integrate(&c, key.clone());
        let arity = key.len() + aggs.len();
        let out = self.push(arity, false, |out| {
            let values = aggs.iter().map(|agg| match agg {
                Agg::Count => "SUM(w)".to_string(),
                Agg::Sum(x) => format!("SUM(c{x} * w)"),
                Agg::Min(x) => format!("MIN(CASE WHEN w > 0 THEN c{x} END)"),
                Agg::Max(x) => format!("MAX(CASE WHEN w > 0 THEN c{x} END)"),
            });
            let select = key.iter().map(|k| format!("c{k}")).chain(values).enumerate().map(|(i, s)| format!("{s} AS c{i}")).collect::<Vec<_>>().join(", ");
            let group = if key.is_empty() { String::new() } else { format!("GROUP BY {}", list("", key.iter().copied())) };
            Some(format!(
                "WITH touched AS ({touched}), cur AS ({cur}) \
                 SELECT {select}, 1 AS w FROM cur {group} HAVING SUM(w) > 0 \
                 UNION ALL SELECT {out_cols}, -w AS w FROM {out_i} o WHERE {hit_o}",
                touched = touched(&c, &key),
                cur = current(&c, &key, "<> 0"),
                out_cols = list("", 0..arity),
                out_i = out.i,
                hit_o = hit("o", &(0..key.len()).collect::<Vec<_>>()),
            ))
        });
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
                "SELECT {dc}, CASE WHEN COALESCE(i.w, 0) + d.w > 0 THEN 1 ELSE -1 END AS w \
                 FROM (SELECT {all}, SUM(w) AS w FROM {d} GROUP BY {all}) d LEFT JOIN {i} i ON {cond} \
                 WHERE (COALESCE(i.w, 0) > 0) <> (COALESCE(i.w, 0) + d.w > 0)",
                dc = n.iter().map(|x| format!("d.c{x} AS c{x}")).collect::<Vec<_>>().join(", "),
                all = list("", 0..c.arity),
                d = c.d,
                i = c.i,
                cond = on("i", &n, "d", &n),
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
                "WITH touched AS ({touched}), cur AS ({cur}), \
                 ranked AS (SELECT {all}, w, SUM(w) OVER ({part} ORDER BY {by} ROWS UNBOUNDED PRECEDING) - w AS before FROM cur) \
                 SELECT {all}, MIN(w, {limit} - before) AS w FROM ranked WHERE before < {limit} \
                 UNION ALL SELECT {all}, -w AS w FROM {out_i} o WHERE {hit_o}",
                touched = touched(&c, &key),
                cur = current(&c, &key, "> 0"),
                out_i = out.i,
                hit_o = hit("o", &key),
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
        Ok(rs)
    }

    fn output(&mut self, rel: RelId, c: Self::C) {
        self.integrate(&c, Vec::new());
        self.outputs.push((rel, c));
    }
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
    pub fn install_on(conn: Connection, p: &Program) -> Result<Self, EngineError> {
        let mut rel = SqlRel::new(p);
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
            for (s, plan) in rel.sccs.iter().enumerate() {
                if plan.at == *k {
                    steps.push(Step::Loop(s));
                }
            }
            match node.owner {
                Owner::Loop(s) => sccs[s].fills.extend(node.fill.clone()),
                _ => steps.extend(node.fill.clone().map(Step::Fill)),
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
                let live = |alias: &str, table: &str| format!("EXISTS (SELECT 1 FROM {table} x WHERE {} AND x.w > 0)", on("x", &all, alias, &all));
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
        conn.execute_batch(&ddl.join(";\n")).map_err(sql_err(Stage::Install))?;
        let engine = Self { conn, tick: 0, sources, steps, sccs, integrates, clears, outputs };
        let all = engine.statements();
        engine.conn.set_prepared_statement_cache_capacity(all.len() + 8);
        for sql in all {
            engine.conn.prepare_cached(sql).map_err(sql_err(Stage::Install))?;
        }
        Ok(engine)
    }

    fn statements(&self) -> Vec<&String> {
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
        all.extend(self.outputs.iter().flat_map(|o| [&o.delta, &o.snapshot]));
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

    fn run(&self, frontier: &Frontier) -> Result<Vec<(RelId, Row, W)>, EngineError> {
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
        self.exec_all(self.integrates.iter().chain(&self.clears))?;
        Ok(changes)
    }
}

impl Engine for Sql {
    fn install(program: &Program) -> Result<Self, EngineError> {
        let conn = Connection::open_in_memory().map_err(sql_err(Stage::Install))?;
        Self::install_on(conn, program)
    }

    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        let settle = sql_err(Stage::Settle);
        self.conn.execute_batch("BEGIN").map_err(&settle)?;
        match self.run(&frontier) {
            Ok(changes) => {
                self.conn.execute_batch("COMMIT").map_err(&settle)?;
                self.tick += 1;
                Ok(Delta { tick: self.tick - 1, changes })
            }
            Err(e) => {
                self.conn.execute_batch("ROLLBACK").map_err(&settle)?;
                Err(e)
            }
        }
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
