//! SQLite engine: `lower` emits one delta table and one INSERT..SELECT per node; all state lives in tables.
//! Join uses the pre-image rule: every node integrates only after every fill of the settle has run.
//! Every read of integrated state is driven from a delta (CROSS JOIN fixes the loop order); aliases of
//! integrated tables end in `_i` so query plans name them.

use ivm_engine::{lower, lower_node, EngineError, ErrorKind, Rel, Stage};
use ivm_ir::*;
use sqlite_ext::rusqlite::{self, Connection};

fn decl(arity: usize) -> String {
    (0..arity)
        .map(|x| format!("c{x} INTEGER NOT NULL"))
        .collect::<Vec<_>>()
        .join(", ")
}

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
    constructors: std::collections::BTreeMap<RelId, (String, Vec<Ty>)>,
}

fn list(alias: &str, cols: impl IntoIterator<Item = usize>) -> String {
    let dot = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    cols.into_iter()
        .map(|c| format!("{dot}c{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `a.cX = b.cY AND ...`, or `1` when there are no pairs.
fn on(a: &str, ac: &[usize], b: &str, bc: &[usize]) -> String {
    if ac.is_empty() {
        return "1".into();
    }
    ac.iter()
        .zip(bc)
        .map(|(x, y)| format!("{a}.c{x} = {b}.c{y}"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn unsupported(what: &'static str) -> EngineError {
    EngineError::new(Stage::Install, None, ErrorKind::Unsupported(what))
}

/// Expr as SQL text; comparisons and logic yield 0/1, Add/Sub wrap like `eval`.
fn render(e: &Expr, arity: usize, maps: &[String]) -> Result<String, EngineError> {
    Ok(match e {
        Expr::Col(c) if (*c as usize) < arity => format!("c{c}"),
        Expr::Col(c) => maps
            .get(*c as usize - arity)
            .cloned()
            .ok_or_else(|| unsupported("Mfp column out of range"))?,
        Expr::Lit(v) if *v == i64::MIN => I64_MIN.into(),
        Expr::Lit(v) => format!("({v})"),
        Expr::Call(func, args) => {
            let a = |i: usize| -> Result<String, EngineError> {
                let arg = args.get(i).ok_or_else(|| unsupported("Func arity"))?;
                render(arg, arity, maps)
            };
            let bin = |op: &str| -> Result<String, EngineError> {
                Ok(format!("(({}) {op} ({}))", a(0)?, a(1)?))
            };
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
                Func::TermLt => format!("ivm_term_lt({}, {})", a(0)?, a(1)?),
            }
        }
    })
}

/// Distinct key values of `c`'s delta as `c0..`; one row or none for an empty key.
fn touched(c: &SqlC, key: &[usize]) -> String {
    if key.is_empty() {
        return format!("(SELECT 1 FROM {} LIMIT 1) t", c.d);
    }
    format!(
        "(SELECT DISTINCT {} FROM {}) t",
        key.iter()
            .enumerate()
            .map(|(t, k)| format!("c{k} AS c{t}"))
            .collect::<Vec<_>>()
            .join(", "),
        c.d
    )
}

/// Rows of `table` whose `at` columns equal a key of `c`'s delta, read by index from the touched keys.
fn of_touched(
    c: &SqlC,
    key: &[usize],
    table: &str,
    at: &[usize],
    arity: usize,
    sign: &str,
) -> String {
    let t: Vec<usize> = (0..key.len()).collect();
    format!(
        "SELECT {}, {sign}x_i.w AS w FROM {} CROSS JOIN {table} x_i ON {}",
        (0..arity)
            .map(|x| format!("x_i.c{x} AS c{x}"))
            .collect::<Vec<_>>()
            .join(", "),
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
            constructors: p.rels.iter().filter(|r| r.kind == RelKind::Constructor)
                .map(|r| (r.id, (r.name.clone(), r.cols.iter().skip(1).copied().collect()))).collect(),
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
    fn push(
        &mut self,
        arity: usize,
        rec: bool,
        body: impl FnOnce(&SqlC) -> Option<String>,
    ) -> SqlC {
        let node = self.nodes.len();
        let c = SqlC {
            node,
            d: format!("ivm_n{node}_d"),
            i: format!("ivm_n{node}_i"),
            arity,
            rec,
        };
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
        self.nodes.push(Node {
            c: c.clone(),
            fill,
            ddl: Vec::new(),
            integrated: false,
            indexes: Vec::new(),
            owner,
        });
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
        let k = self.push(c.arity, false, |_| {
            Some(format!("SELECT {}, w FROM {}", list("", 0..c.arity), c.d))
        });
        self.nodes[k.node].owner = Owner::Copy(scc);
        self.integrate(&k, Vec::new());
        self.active
            .as_mut()
            .unwrap()
            .copies
            .push((c.node, k.clone()));
        k
    }

    fn feed(&mut self, cs: Vec<SqlC>) -> Vec<SqlC> {
        if !cs.iter().any(|c| c.rec) {
            return cs;
        }
        cs.into_iter()
            .map(|c| if c.rec { c } else { self.copy(c) })
            .collect()
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
        if let Some((name, types)) = self.constructors.get(&rel).cloned() {
            let table = crate::terms::ctor_table(&name);
            let width = types.len() + 1;
            let c = self.push(width, false, |new| Some(format!(
                "SELECT {}, 1 AS w FROM {} t WHERE NOT EXISTS (SELECT 1 FROM {} x_i WHERE x_i.c0=t.c0)",
                (0..width).map(|i| format!("t.c{i} AS c{i}")).collect::<Vec<_>>().join(", "),
                crate::catalog::quote(&table), new.i,
            )));
            self.integrate(&c, vec![0]);
            return Ok(c);
        }
        self.sources
            .iter()
            .find(|(id, _)| *id == rel)
            .map(|(_, c)| c.clone())
            .ok_or_else(|| EngineError::new(Stage::Install, Some(rel), ErrorKind::UnknownRel(rel)))
    }

    fn mint(&mut self, c: Self::C, functor: RelId, args: &[ColId]) -> Result<Self::C, EngineError> {
        let (name, types) = self.constructors.get(&functor)
            .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
        if args.len() != types.len() {
            return Err(EngineError::new(Stage::Install, Some(functor), ErrorKind::Arity { expected: types.len(), actual: args.len() }));
        }
        if args.iter().any(|x| *x as usize >= c.arity) {
            return Err(unsupported("Mint column out of range"));
        }
        self.flat(&c, "Mint over a LetRec variable");
        let functor = name.replace('\'', "''");
        let types_json = serde_json::to_string(&types).expect("Ty serialization").replace('\'', "''");
        let arg_expr = format!("json_array({})", args.iter().map(|x| format!("d.c{x}")).collect::<Vec<_>>().join(","));
        let arg_cols = args.iter().enumerate().map(|(i, x)| format!("d.c{x} AS c{}", i + 1)).collect::<Vec<_>>().join(",");
        let ctor = crate::catalog::quote(crate::terms::ctor_table(&name));
        let old = c.arity;
        let next = self.push(old + 1, false, |_| Some(format!(
            "SELECT {}, dict.id AS c{old}, d.w FROM {} d JOIN ivm_term_dict dict ON dict.functor='{functor}' AND dict.args={arg_expr}",
            (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(","), c.d,
        )));
        let insert_dict = format!(
            "INSERT INTO ivm_term_dict(functor,args,types) SELECT '{functor}', {arg_expr}, '{types_json}' FROM {} d WHERE d.w>0 AND NOT EXISTS (SELECT 1 FROM ivm_term_dict x WHERE x.functor='{functor}' AND x.args={arg_expr}) GROUP BY {arg_expr} ORDER BY {arg_expr} ON CONFLICT(functor,args) DO NOTHING", c.d,
        );
        let insert_ctor = format!(
            "INSERT INTO {ctor} SELECT dict.id AS c0{} FROM {} d JOIN ivm_term_dict dict ON dict.functor='{functor}' AND dict.args={arg_expr} WHERE d.w>0 GROUP BY {arg_expr} ORDER BY {arg_expr} ON CONFLICT(c0) DO NOTHING",
            if arg_cols.is_empty() { String::new() } else { format!(",{arg_cols}") }, c.d,
        );
        self.nodes[next.node].fill.splice(0..0, [insert_dict, insert_ctor]);
        Ok(next)
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C {
        let rendered = (|| -> Result<(Vec<String>, Vec<String>), EngineError> {
            let mut maps = Vec::new();
            for e in map {
                let m = render(e, c.arity, &maps)?;
                maps.push(m);
            }
            let wheres = filter
                .iter()
                .map(|e| Ok(format!("({}) <> 0", render(e, c.arity, &[])?)))
                .collect::<Result<_, _>>()?;
            let all: Vec<ColId> = (0..(c.arity + maps.len()) as ColId).collect();
            let proj = if project.is_empty() {
                &all[..]
            } else {
                project
            };
            let cols = proj
                .iter()
                .map(|p| render(&Expr::Col(*p), c.arity, &maps))
                .collect::<Result<_, _>>()?;
            Ok((wheres, cols))
        })();
        let (wheres, cols) = rendered.unwrap_or_else(|e| {
            self.fail(e);
            (Vec::new(), vec!["0".into()])
        });
        self.push(cols.len(), c.rec, |_| {
            let select = cols
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{s} AS c{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let wheres = if wheres.is_empty() {
                "1".into()
            } else {
                wheres.join(" AND ")
            };
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
        self.push(arity, rec, |_| {
            Some(
                cs.iter()
                    .map(|c| format!("SELECT {cols}, w FROM {}", c.d))
                    .collect::<Vec<_>>()
                    .join(" UNION ALL "),
            )
        })
    }

    fn negate(&mut self, c: Self::C) -> Self::C {
        self.flat(&c, "Negate over a LetRec variable");
        self.push(c.arity, false, |_| {
            Some(format!(
                "SELECT {}, -w AS w FROM {}",
                list("", 0..c.arity),
                c.d
            ))
        })
    }

    /// Δ(a⋈b) = Δa⋈Δb + I⁻(a)⋈Δb + Δa⋈I⁻(b), with I⁻ the integrated tables not yet updated this settle.
    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>]) -> Result<Self::C, EngineError> {
        let [a, b]: [SqlC; 2] = self
            .feed(cs)
            .try_into()
            .map_err(|_| unsupported("Join arity != 2"))?;
        let side = |input: u8| -> Result<Vec<usize>, EngineError> {
            eq.iter()
                .map(|class| {
                    class
                        .iter()
                        .find(|(i, _)| *i == input)
                        .map(|(_, c)| *c as usize)
                })
                .collect::<Option<_>>()
                .ok_or_else(|| unsupported("Join class missing a side"))
        };
        let (lk, rk) = (side(0)?, side(1)?);
        self.integrate(&a, lk.clone());
        self.integrate(&b, rk.clone());
        let term = |l: &str, r: &str, l_first: bool| {
            let (la, ra) = (
                if l.ends_with("_i") { "l_i" } else { "l_d" },
                if r.ends_with("_i") { "r_i" } else { "r_d" },
            );
            let from = if l_first {
                format!("{l} {la} CROSS JOIN {r} {ra}")
            } else {
                format!("{r} {ra} CROSS JOIN {l} {la}")
            };
            format!(
                "SELECT {}, {}, {la}.w * {ra}.w AS w FROM {from} ON {}",
                (0..a.arity)
                    .map(|i| format!("{la}.c{i} AS c{i}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                (0..b.arity)
                    .map(|i| format!("{ra}.c{i} AS c{}", a.arity + i))
                    .collect::<Vec<_>>()
                    .join(", "),
                on(la, &lk, ra, &rk)
            )
        };
        let terms = [
            term(&a.d, &b.d, true),
            term(&a.i, &b.d, false),
            term(&a.d, &b.i, true),
        ];
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
        let keys = if rk.is_empty() {
            self.mfp(r, &[], &[Expr::Lit(0)], &unit)
        } else {
            self.mfp(r, &[], &[], rk)
        };
        let keys = self.threshold(keys);
        let eq: Vec<Vec<(u8, ColId)>> = lk
            .iter()
            .enumerate()
            .map(|(i, c)| vec![(0, *c), (1, i as ColId)])
            .collect();
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
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg], input_types: &[Ty]) -> Self::C {
        self.flat(&c, "Reduce over a LetRec variable");
        let key: Vec<usize> = key.iter().map(|k| *k as usize).collect();
        let arity = key.len() + aggs.len();
        let out = self.push(arity, false, |_| None);
        let k = out.node;
        let (g, arr) = (format!("ivm_n{k}_g"), format!("ivm_n{k}_in"));
        let gk: Vec<usize> = (0..key.len().max(1)).collect();
        let gcols = gk
            .iter()
            .map(|x| format!("g{x}"))
            .collect::<Vec<_>>()
            .join(", ");
        let kx = if key.is_empty() {
            vec!["0".to_string()]
        } else {
            key.iter().map(|x| format!("c{x}")).collect()
        };
        let group = if key.is_empty() {
            "HAVING COUNT(*) > 0".to_string()
        } else {
            format!("GROUP BY {}", kx.join(", "))
        };
        let sums: Vec<(usize, ColId)> = aggs
            .iter()
            .enumerate()
            .filter_map(|(j, a)| {
                if let Agg::Sum(x) = a {
                    Some((j, *x))
                } else {
                    None
                }
            })
            .collect();
        let extremes: Vec<ColId> = aggs
            .iter()
            .filter_map(|a| {
                if let Agg::Min(x) | Agg::Max(x) = a {
                    Some(*x)
                } else {
                    None
                }
            })
            .collect();
        let s_cols: String = sums.iter().map(|(j, _)| format!(", a{j}")).collect();
        let mut ddl =
            vec![format!(
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
            ddl.push(format!(
                "CREATE TABLE {arr} ({}, w INTEGER NOT NULL, PRIMARY KEY ({all})) WITHOUT ROWID",
                decl(c.arity)
            ));
            for x in &extremes {
                let cols = key
                    .iter()
                    .chain([&(*x as usize)])
                    .map(|x| format!("c{x}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                ddl.push(format!(
                    "CREATE INDEX IF NOT EXISTS ivm_n{k}_in{x} ON {arr} ({cols}, w)"
                ));
            }
            fill.push(format!(
                "INSERT INTO {arr} ({all}, w) SELECT {all}, SUM(w) FROM {d} WHERE true GROUP BY {all} ON CONFLICT ({all}) DO UPDATE SET w = w + excluded.w",
                d = c.d
            ));
            fill.push(format!(
                "DELETE FROM {arr} WHERE w = 0 AND ({all}) IN (SELECT {all} FROM {d})",
                d = c.d
            ));
        }
        let g_on = |alias: &str| {
            let pairs: Vec<String> = key
                .iter()
                .zip(&gk)
                .map(|(x, t)| format!("n_i.c{x} = {alias}.g{t}"))
                .collect();
            if pairs.is_empty() {
                "1".to_string()
            } else {
                pairs.join(" AND ")
            }
        };
        let values = aggs.iter().enumerate().map(|(j, agg)| match agg {
            Agg::Count => "g_i.cnt".to_string(),
            Agg::Sum(_) => format!("g_i.a{j}"),
            Agg::Min(x) => format!("(SELECT n_i.c{x} FROM {arr} n_i WHERE {} AND n_i.w > 0 ORDER BY {} LIMIT 1)", g_on("g_i"),
                if input_types[*x as usize] == Ty::Id { format!("ivm_term_key(n_i.c{x})") } else { format!("n_i.c{x}") }),
            Agg::Max(x) => format!("(SELECT n_i.c{x} FROM {arr} n_i WHERE {} AND n_i.w > 0 ORDER BY {} DESC LIMIT 1)", g_on("g_i"),
                if input_types[*x as usize] == Ty::Id { format!("ivm_term_key(n_i.c{x})") } else { format!("n_i.c{x}") }),
        });
        let select = gk
            .iter()
            .take(key.len())
            .map(|x| format!("g_i.g{x}"))
            .chain(values)
            .enumerate()
            .map(|(i, s)| format!("{s} AS c{i}"))
            .collect::<Vec<_>>()
            .join(", ");
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
            sums.iter()
                .map(|(j, _)| format!(" AND a{j} = 0"))
                .collect::<String>(),
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
    fn topk(
        &mut self,
        c: Self::C,
        key: &[ColId],
        order: &[Order],
        limit: u32,
        input_types: &[Ty],
    ) -> Result<Self::C, EngineError> {
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
                .map(|o| {
                    let x = o.col as usize;
                    let value = if input_types[x] == Ty::Id { format!("ivm_term_key(c{x})") } else { format!("c{x}") };
                    format!("{value}{}", if o.desc { " DESC" } else { "" })
                })
                .chain((0..c.arity).map(|x| if input_types[x] == Ty::Id { format!("ivm_term_key(c{x})") } else { format!("c{x}") }))
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

    /// Recompute only partitions named by this frontier's input delta.
    fn window(
        &mut self,
        c: Self::C,
        partition: &[ColId],
        order: &[Order],
        func: &WinFn,
        input_types: &[Ty],
    ) -> Result<Self::C, EngineError> {
        if c.rec {
            return Err(unsupported("Window over a LetRec variable"));
        }
        let key: Vec<usize> = partition.iter().map(|x| *x as usize).collect();
        self.integrate(&c, key.clone());
        let value_col = order.first().map_or(0, |o| o.col as usize);
        let all = list("", 0..c.arity);
        let part = if key.is_empty() { String::new() } else { format!("PARTITION BY {}", list("", key.iter().copied())) };
        let order_cols = order.iter().map(|o| {
            let x = o.col as usize;
            let value = if input_types[x] == Ty::Id { format!("ivm_term_key(c{x})") } else { format!("c{x}") };
            format!("{value}{}", if o.desc { " DESC" } else { " ASC" })
        }).collect::<Vec<_>>();
        let by = order_cols.iter().cloned()
            .chain((0..c.arity).map(|x| if input_types[x] == Ty::Id { format!("ivm_term_key(c{x}) ASC") } else { format!("c{x} ASC") }))
            .chain(["seq ASC".to_string()]).collect::<Vec<_>>().join(", ");
        let order_clause = if order_cols.is_empty() { String::new() } else { format!("ORDER BY {}", order_cols.join(", ")) };
        let full_clause = if part.is_empty() { format!("ORDER BY {by}") } else { format!("{part} ORDER BY {by}") };
        let rank_clause = [part.as_str(), order_clause.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        let aggregate_clause = if order_cols.is_empty() { part.clone() } else { format!("{full_clause} ROWS UNBOUNDED PRECEDING") };
        let value = match func {
            WinFn::RowNumber => format!("ROW_NUMBER() OVER ({full_clause})"),
            WinFn::Rank => format!("RANK() OVER ({rank_clause})"),
            WinFn::DenseRank => format!("DENSE_RANK() OVER ({rank_clause})"),
            WinFn::Lag(offset) => format!("COALESCE(LAG(c{value_col}, {offset}, 0) OVER ({full_clause}), 0)"),
            WinFn::Lead(offset) => format!("COALESCE(LEAD(c{value_col}, {offset}, 0) OVER ({full_clause}), 0)"),
            WinFn::Sum(col) => format!("SUM(c{col}) OVER ({aggregate_clause})"),
            WinFn::Count => format!("COUNT(*) OVER ({aggregate_clause})"),
        };
        let out = self.push(c.arity + 1, false, |out| {
            let gone = of_touched(&c, &key, &out.i, &key, c.arity + 1, "-");
            Some(format!(
                "WITH RECURSIVE cur AS (SELECT {all}, SUM(w) AS w FROM ({old} UNION ALL SELECT {all}, w FROM {d}) GROUP BY {all} HAVING SUM(w) > 0), \
                 expanded AS (SELECT {all}, w, 1 AS seq FROM cur UNION ALL SELECT {all}, w, seq + 1 FROM expanded WHERE seq < w), \
                 ranked AS (SELECT {all}, {value} AS c{width} FROM expanded) \
                 SELECT {all}, c{width}, 1 AS w FROM ranked UNION ALL {gone}",
                old = of_touched(&c, &key, &c.i, &key, c.arity, ""),
                d = c.d,
                width = c.arity,
            ))
        });
        self.integrate(&out, key);
        Ok(out)
    }

    /// Variables are SCC-owned tables maintained by a DRed round loop at settle; see `Sql::fixpoint`.
    fn letrec(
        &mut self,
        p: &Program,
        rec: &LetRec,
        defined: &[(RelId, Self::C)],
    ) -> Result<Vec<Self::C>, EngineError> {
        if self.active.is_some() {
            return Err(unsupported("LetRec nested in LetRec"));
        }
        if rec.limit.is_some() {
            return Err(unsupported("LetRec limit"));
        }
        let scc = self.sccs.len();
        self.active = Some(Active {
            scc,
            copies: Vec::new(),
        });
        let mut scope: Vec<(RelId, SqlC)> = defined.to_vec();
        let mut vs = Vec::new();
        for id in &rec.ids {
            let rel = p.rel(*id).ok_or_else(|| {
                EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id))
            })?;
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
            return Err(unsupported(
                "LetRec ids and bodies differ in count or arity",
            ));
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

pub(crate) struct NodesPlan {
    pub ddl: Vec<String>,
    pub objects: Vec<(String, String)>,
    source_fills: Vec<String>,
    steps: Vec<Step>,
    sccs: Vec<SccSql>,
    integrates: Vec<String>,
    clears: Vec<String>,
    pub output_delta: String,
    pub output_snapshot: String,
    pub output_arity: usize,
}

impl NodesPlan {
    /// Statements issued during settle, excluding snapshot reads.
    pub fn statements(&self) -> Vec<&str> {
        let mut all: Vec<&str> = self.source_fills.iter().map(String::as_str).collect();
        all.extend(self.steps.iter().filter_map(|step| match step {
            Step::Fill(sql) => Some(sql.as_str()),
            Step::Loop(_) => None,
        }));
        for scc in &self.sccs {
            all.extend(scc.fills.iter().chain(&scc.integrates).chain(&scc.clears).chain(&scc.stash).chain(&scc.restore).map(String::as_str));
            for v in &scc.vars {
                all.extend([
                    &v.over_delete, &v.rederive, &v.insert, &v.to_delta, &v.to_acc,
                    &v.to_del, &v.clear_nx, &v.clear_del, &v.any, &v.finish, &v.clear_acc,
                ].into_iter().map(String::as_str));
            }
        }
        all.extend(self.integrates.iter().chain(&self.clears).map(String::as_str));
        all.push(&self.output_delta);
        all
    }

    pub fn compile(name: &str, program: &Program) -> Result<Self, EngineError> {
        let mut rel = SqlRel::new(program, false);
        lower(program, &mut rel)?;
        if let Some(error) = rel.err {
            return Err(error);
        }
        let mut ddl = Vec::new();
        let mut objects = Vec::new();
        let mut source_fills = Vec::new();
        let (mut steps, mut integrates, mut clears) = (Vec::new(), Vec::new(), Vec::new());
        let mut sccs: Vec<SccSql> = rel.sccs.iter().map(|_| SccSql::default()).collect();
        for node in &rel.nodes {
            let SqlC {
                node: k,
                d,
                i,
                arity,
                ..
            } = &node.c;
            let cols = list("", 0..*arity);
            ddl.push(format!(
                "CREATE TABLE {d} ({}, w INTEGER NOT NULL)",
                decl(*arity)
            ));
            objects.push(("TABLE".into(), d.clone()));
            ddl.extend(node.ddl.iter().cloned());
            for extra in &node.ddl {
                if let Some((kind, name)) = ddl_object(extra) {
                    objects.push((kind, name));
                }
            }
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
                ddl.push(format!(
                    "CREATE TABLE {hold} ({}, w INTEGER NOT NULL)",
                    decl(*arity)
                ));
                objects.push(("TABLE".into(), hold.clone()));
                sccs[s].stash.extend([
                    format!("INSERT INTO {hold} SELECT * FROM {d} WHERE w > 0"),
                    format!("DELETE FROM {d} WHERE w > 0"),
                ]);
                sccs[s].restore.extend([
                    format!("INSERT INTO {d} SELECT * FROM {hold}"),
                    format!("DELETE FROM {hold}"),
                ]);
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
                ddl.push(format!(
                    "CREATE TABLE {i} ({}, w INTEGER NOT NULL, PRIMARY KEY ({cols})) WITHOUT ROWID",
                    decl(*arity)
                ));
                objects.push(("TABLE".into(), i.clone()));
                for (j, index) in node.indexes.iter().enumerate() {
                    let ix = format!("ivm_n{k}_x{j}");
                    ddl.push(format!(
                        "CREATE INDEX {ix} ON {i} ({})",
                        list("", index.iter().copied())
                    ));
                    objects.push(("INDEX".into(), ix));
                }
                ints.push(format!("INSERT INTO {i} ({cols}, w) SELECT {cols}, SUM(w) FROM {d} WHERE true GROUP BY {cols} \
                    ON CONFLICT ({cols}) DO UPDATE SET w = w + excluded.w"));
                ints.push(format!(
                    "DELETE FROM {i} WHERE w = 0 AND ({cols}) IN (SELECT {cols} FROM {d})"
                ));
            }
        }
        for (plan, scc) in rel.sccs.iter().zip(&mut sccs) {
            for (v, b, r) in &plan.vars {
                let n = v.arity;
                let (cols, all) = (list("", 0..n), (0..n).collect::<Vec<_>>());
                let (nx, del, acc) = (
                    format!("ivm_n{}_nx", v.node),
                    format!("ivm_n{}_del", v.node),
                    format!("ivm_n{}_acc", r.node),
                );
                for t in [&nx, &del, &acc] {
                    ddl.push(format!(
                        "CREATE TABLE {t} ({}, w INTEGER NOT NULL)",
                        decl(n)
                    ));
                    objects.push(("TABLE".into(), t.clone()));
                }
                let live = |alias: &str, table: &str| {
                    format!(
                        "EXISTS (SELECT 1 FROM {table} x_i WHERE {} AND x_i.w > 0)",
                        on("x_i", &all, alias, &all)
                    )
                };
                scc.vars.push(VarSql {
                    over_delete: format!("INSERT INTO {nx} SELECT {cols}, -1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w < 0) t WHERE {}", live("t", &v.i), bd=b.d),
                    rederive: format!("INSERT INTO {nx} SELECT {cols}, 1 FROM {del} t WHERE {}", live("t", &b.i)),
                    insert: format!("INSERT INTO {nx} SELECT {cols}, 1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w > 0) t WHERE {} AND NOT {}", live("t", &b.i), live("t", &v.i), bd=b.d),
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
        for (id, c) in &rel.sources {
            let source = program.rel(*id).ok_or_else(|| {
                EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id))
            })?;
            let cols = list("", 0..c.arity);
            let vals = (0..c.arity)
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(",");
            source_fills.push(format!("INSERT INTO {} ({cols},w) SELECT {vals},SUM(__sign) FROM {} WHERE __table='{}' GROUP BY {vals} HAVING SUM(__sign)<>0",
                c.d, crate::catalog::quote(crate::catalog::stage(name)), source.name.replace('\'', "''")));
        }
        let (_, output) = rel
            .outputs
            .first()
            .ok_or_else(|| unsupported("no output"))?;
        if rel.outputs.len() != 1 {
            return Err(unsupported("multiple output relations"));
        }
        let cols = list("", 0..output.arity);
        let mut plan = Self {
            ddl,
            objects,
            source_fills,
            steps,
            sccs,
            integrates,
            clears,
            output_delta: format!(
                "SELECT {cols},SUM(w) FROM {} GROUP BY {cols} HAVING SUM(w)<>0 ORDER BY {cols}",
                output.d
            ),
            output_snapshot: format!(
                "SELECT {cols},w FROM {} WHERE w>0 ORDER BY {cols}",
                output.i
            ),
            output_arity: output.arity,
        };
        plan.prefix(name);
        Ok(plan)
    }

    fn prefix(&mut self, program: &str) {
        let prefix = format!("frontier_{program}_n");
        let fix = |s: &mut String| *s = s.replace("ivm_n", &prefix);
        self.ddl.iter_mut().for_each(&fix);
        for (_, name) in &mut self.objects {
            fix(name);
        }
        self.source_fills.iter_mut().for_each(&fix);
        for step in &mut self.steps {
            if let Step::Fill(sql) = step {
                fix(sql);
            }
        }
        for scc in &mut self.sccs {
            scc.fills.iter_mut().for_each(&fix);
            scc.integrates.iter_mut().for_each(&fix);
            scc.clears.iter_mut().for_each(&fix);
            scc.stash.iter_mut().for_each(&fix);
            scc.restore.iter_mut().for_each(&fix);
            for var in &mut scc.vars {
                for sql in [
                    &mut var.over_delete,
                    &mut var.rederive,
                    &mut var.insert,
                    &mut var.to_delta,
                    &mut var.to_acc,
                    &mut var.to_del,
                    &mut var.clear_nx,
                    &mut var.clear_del,
                    &mut var.any,
                    &mut var.finish,
                    &mut var.clear_acc,
                ] {
                    fix(sql);
                }
            }
        }
        self.integrates.iter_mut().for_each(&fix);
        self.clears.iter_mut().for_each(&fix);
        fix(&mut self.output_delta);
        fix(&mut self.output_snapshot);
    }

    fn exec(db: &Connection, sql: &str) -> rusqlite::Result<usize> {
        db.prepare_cached(sql)?.execute([])
    }

    fn exec_all<'a>(
        db: &Connection,
        sqls: impl IntoIterator<Item = &'a String>,
    ) -> rusqlite::Result<()> {
        for sql in sqls {
            Self::exec(db, sql)?;
        }
        Ok(())
    }

    fn round(&self, db: &Connection, scc: &SccSql, deleting: bool) -> rusqlite::Result<bool> {
        Self::exec_all(db, scc.fills.iter().chain(&scc.integrates))?;
        Self::exec_all(
            db,
            scc.vars
                .iter()
                .map(|v| if deleting { &v.over_delete } else { &v.insert }),
        )?;
        Self::exec_all(db, &scc.clears)?;
        let mut more = false;
        for v in &scc.vars {
            Self::exec_all(db, [&v.to_delta, &v.to_acc])?;
            if deleting {
                Self::exec(db, &v.to_del)?;
            }
            Self::exec(db, &v.clear_nx)?;
            more |= db
                .prepare_cached(&v.any)?
                .query_row([], |r| r.get::<_, bool>(0))?;
        }
        Ok(more)
    }

    fn fixpoint(&self, db: &Connection, scc: &SccSql) -> rusqlite::Result<()> {
        Self::exec_all(db, &scc.stash)?;
        while self.round(db, scc, true)? {}
        for v in &scc.vars {
            Self::exec_all(
                db,
                [
                    &v.rederive,
                    &v.clear_del,
                    &v.to_delta,
                    &v.to_acc,
                    &v.clear_nx,
                ],
            )?;
        }
        Self::exec_all(db, &scc.restore)?;
        while self.round(db, scc, false)? {}
        for v in &scc.vars {
            Self::exec_all(db, [&v.finish, &v.clear_acc])?;
        }
        Ok(())
    }

    pub fn run(&self, db: &Connection) -> rusqlite::Result<Vec<(Row, W)>> {
        Self::exec_all(db, &self.source_fills)?;
        for step in &self.steps {
            match step {
                Step::Fill(sql) => {
                    Self::exec(db, sql)?;
                }
                Step::Loop(s) => self.fixpoint(db, &self.sccs[*s])?,
            }
        }
        let mut stmt = db.prepare_cached(&self.output_delta)?;
        let changes = stmt
            .query_map([], |r| {
                Ok((
                    (0..self.output_arity)
                        .map(|i| r.get(i))
                        .collect::<Result<Row, _>>()?,
                    r.get(self.output_arity)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Self::exec_all(db, self.integrates.iter().chain(&self.clears))?;
        Ok(changes)
    }
}

fn ddl_object(sql: &str) -> Option<(String, String)> {
    let words = sql.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["CREATE", kind @ ("TABLE" | "INDEX"), name, ..] => Some(((*kind).into(), (*name).into())),
        _ => None,
    }
}
