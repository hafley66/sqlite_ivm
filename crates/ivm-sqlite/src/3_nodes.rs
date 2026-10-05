//! SQLite engine: `lower` keeps persistent images in tables and folds pure deltas into CTEs.
//! Join uses the pre-image rule: every node integrates only after every fill of the settle has run.
//! Every read of integrated state is driven from a delta (CROSS JOIN fixes the loop order); aliases of
//! integrated tables end in `_i` so query plans name them.

use ivm_engine::{lower, lower_node, Counters, EngineError, ErrorKind, Rel, Stage};
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

/// Equality of two typed cells. Id and Text cells are hash-consed (`UNIQUE` constructor arguments,
/// `UNIQUE` text), so equal terms have equal ids and the plain integer `=` holds; the planner can then
/// probe the key index. Other pairs compare their ordered values. `op` is `=` or `<>`.
fn equal(op: &str, lt: Ty, left: String, rt: Ty, right: String) -> String {
    match (lt, rt) {
        (Ty::Id, Ty::Id) | (Ty::Text, Ty::Text) | (Ty::Int, Ty::Int) => format!("{left} {op} {right}"),
        _ => format!("{} {op} {}", ordered(lt, left), ordered(rt, right)),
    }
}

fn ordered(ty: Ty, value: String) -> String {
    match ty {
        Ty::Id => crate::terms::sort_key_sql(&value),
        Ty::Text => format!("ivm_text_value({value})"),
        Ty::Real => format!("ivm_real_value({value})"),
        Ty::Any => format!("ivm_any_value({value})"),
        Ty::Int => value,
    }
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkKind { Filter, Join, Antijoin, Reduce, Topk, Window, Mint }

impl WorkKind {
    fn of(op: &Op) -> Option<Self> {
        Some(match op {
            Op::Mfp { .. } => Self::Filter,
            Op::Join { .. } => Self::Join,
            Op::Antijoin { .. } => Self::Antijoin,
            Op::Reduce { .. } => Self::Reduce,
            Op::TopK { .. } => Self::Topk,
            Op::Window { .. } => Self::Window,
            Op::Mint { .. } | Op::StrCons { .. } | Op::Str { .. } => Self::Mint,
            _ => return None,
        })
    }

    fn add(self, counters: &mut Counters, rows: u64) {
        let field = match self {
            Self::Filter => &mut counters.delta_rows.filter,
            Self::Join => &mut counters.delta_rows.join,
            Self::Antijoin => &mut counters.delta_rows.antijoin,
            Self::Reduce => &mut counters.delta_rows.reduce,
            Self::Topk => &mut counters.delta_rows.topk,
            Self::Window => &mut counters.delta_rows.window,
            Self::Mint => &mut counters.delta_rows.mint,
        };
        *field.as_mut().unwrap() += rows;
    }
}

/// Physical images one view may read: nested views flatten into the reader's join (SQLite joins at
/// most 64 tables), and the bound keeps each reader's plan small.
const VIEW_TABLES: usize = 6;

/// Statements that may each regroup one source's delta from the stage; a source read by more
/// keeps a `_d` table filled once.
const SOURCE_READS: usize = 16;

/// A node's image computed from its inputs' images: `body` reads `ivm_nX_i` of each input; `pass`
/// names, per output column, the input node and column it copies.
struct View {
    body: String,
    inputs: Vec<usize>,
    pass: Vec<Option<(usize, usize)>>,
    tables: usize,
}

struct Node {
    c: SqlC,
    body: Option<String>,
    inline: bool,
    /// Join nodes may turn their delta into a CTE when one statement reads it.
    join: bool,
    union: bool,
    /// A source delta is a CTE over the stage.
    source: bool,
    image: Option<View>,
    /// A join term read `image` in place of a stored `_i`.
    viewed: bool,
    fill: Vec<String>,
    delete_fill: Vec<String>,
    insert_fill: Vec<String>,
    delete_seed: Vec<String>,
    insert_seed: Vec<String>,
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
    /// Recomputed from empty variables at every settle that changes an input; see `Sql::recompute`.
    nonmonotone: bool,
    limit: Option<u32>,
    first: Option<RelId>,
}

struct Active {
    scc: usize,
    copies: Vec<(usize, SqlC)>,
    nonmonotone: bool,
}

/// A LetRec whose body may negate, aggregate or retract a variable, or that carries a limit.
/// Such a body is outside the DRed contract (a deleted row's support can come back through a
/// negation), so the LetRec takes the recompute path. Post-order over the nodes the bodies reach.
fn nonmonotone(p: &Program, rec: &LetRec) -> bool {
    if rec.limit.is_some() { return true; }
    let inputs = |op: &Op| -> Vec<NodeId> {
        match op {
            Op::Get(_) | Op::Delay(_) => Vec::new(),
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Negate(input) | Op::Reduce { input, .. } | Op::Threshold(input)
            | Op::TopK { input, .. } | Op::Window { input, .. } => vec![*input],
            Op::Union(inputs) | Op::Join { inputs, .. } => inputs.clone(),
            Op::Antijoin { l, r, .. } => vec![*l, *r],
        }
    };
    let mut dependent: Vec<Option<bool>> = vec![None; p.nodes.len()];
    let mut stack: Vec<(NodeId, bool)> = rec.bodies.iter().map(|body| (*body, false)).collect();
    while let Some((id, expanded)) = stack.pop() {
        let Some(op) = p.nodes.get(id as usize) else { continue };
        if dependent[id as usize].is_some() { continue; }
        let ins = inputs(op);
        if !expanded {
            stack.push((id, true));
            stack.extend(ins.iter().filter(|n| dependent.get(**n as usize).is_some_and(Option::is_none)).map(|n| (*n, false)));
            continue;
        }
        let dep = |n: &NodeId| dependent.get(*n as usize).copied().flatten() == Some(true);
        let reads_rec = matches!(op, Op::Get(rel) if rec.ids.contains(rel));
        let negative = match op {
            Op::Negate(input) | Op::Threshold(input) | Op::Reduce { input, .. } => dep(input),
            Op::Antijoin { r, .. } => dep(r),
            _ => false,
        };
        if negative { return true; }
        dependent[id as usize] = Some(reads_rec || ins.iter().any(dep));
    }
    false
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
    work_nodes: Vec<(NodeId, usize)>,
    /// Join SQL node -> its three delta terms, each consolidated: Δa⋈Δb, I⁻a⋈Δb, Δa⋈I⁻b.
    terms: Vec<(usize, Vec<String>)>,
    constructors: std::collections::BTreeMap<RelId, (String, Vec<Ty>)>,
    texts: Vec<String>,
    /// Join SQL node per (left node, right node, predicates): rules that join the same inputs on the
    /// same keys share one plan node.
    joins: std::collections::HashMap<(usize, usize, Vec<String>), SqlC>,
    /// `Program::node_types_memo` answers for the one program this plan lowers.
    types: Vec<Option<Option<Vec<Ty>>>>,
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

/// Expr as SQL text; comparisons and logic yield 0/1, Add/Sub wrap like `eval`. Renders the tree
/// in post-order with an explicit stack: each call reads its arguments' rendered text.
fn render(e: &Expr, arity: usize, maps: &[String], texts: &[String], types: &[Ty]) -> Result<String, EngineError> {
    // Pre-order with the last argument first; reversed, every argument precedes its call, left to right.
    let mut order = Vec::new();
    let mut walk = vec![e];
    while let Some(next) = walk.pop() {
        order.push(next);
        if let Expr::Call(_, args) = next { walk.extend(args.iter()); }
    }
    let mut done: Vec<String> = Vec::new();
    for e in order.into_iter().rev() {
        let sql = match e {
            Expr::Col(c) if (*c as usize) < arity => format!("c{c}"),
            Expr::Col(c) => maps
                .get(*c as usize - arity)
                .cloned()
                .ok_or_else(|| unsupported("Mfp column out of range"))?,
            Expr::Lit(v) if *v == i64::MIN => I64_MIN.into(),
            Expr::Lit(v) => format!("({v})"),
            Expr::Text(index) => {
                let value = texts.get(*index as usize).ok_or_else(|| unsupported("Text index out of range"))?;
                format!("ivm_text_id('{}')", value.replace('\'', "''"))
            }
            Expr::Call(func, args) => {
                let rendered = done.split_off(done.len() - args.len());
                let a = |i: usize| -> Result<String, EngineError> {
                    rendered.get(i).cloned().ok_or_else(|| unsupported("Func arity"))
                };
                let ty = |i: usize| expr_type(&args[i], types).ok_or_else(|| unsupported("expression type"));
                let bin = |op: &str| -> Result<String, EngineError> {
                    let left = ordered(ty(0)?, a(0)?);
                    let right = ordered(ty(1)?, a(1)?);
                    Ok(format!("({left} {op} {right})"))
                };
                match func {
                    Func::Eq => format!("({})", equal("=", ty(0)?, a(0)?, ty(1)?, a(1)?)),
                    Func::Ne => format!("({})", equal("<>", ty(0)?, a(0)?, ty(1)?, a(1)?)),
                    Func::Lt => bin("<")?,
                    Func::Le => bin("<=")?,
                    Func::Gt => bin(">")?,
                    Func::Ge => bin(">=")?,
                    Func::Add | Func::Sub if expr_type(e, types) == Some(Ty::Real) => {
                        let left = if expr_type(&args[0], types) == Some(Ty::Real) { format!("ivm_real_value({})", a(0)?) } else { a(0)? };
                        let right = if expr_type(&args[1], types) == Some(Ty::Real) { format!("ivm_real_value({})", a(1)?) } else { a(1)? };
                        format!("ivm_real_bits({left} {} {right})", if *func == Func::Add { "+" } else { "-" })
                    }
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
                    Func::StrNil => "ivm_text_id('')".into(),
                }
            }
        };
        done.push(sql);
    }
    done.pop().ok_or_else(|| unsupported("empty expression"))
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
            work_nodes: Vec::new(),
            terms: Vec::new(),
            constructors: p.rels.iter().filter(|r| r.kind == RelKind::Constructor)
                .map(|r| (r.id, (r.name.clone(), r.cols.iter().skip(1).copied().collect()))).collect(),
            texts: p.texts.clone(),
            joins: std::collections::HashMap::new(),
            types: Vec::new(),
        };
        for r in p.rels.iter().filter(|r| r.kind == RelKind::Source) {
            let c = rel.push(r.cols.len(), false, |_| None);
            rel.integrate(&c, (0..c.arity).collect());
            rel.nodes[c.node].source = true;
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
        let body = body(&c);
        let fill = body.iter().map(|body| {
            let cols = list("", 0..arity);
            format!(
                "INSERT INTO {d} ({cols}, w) SELECT {cols}, SUM(w) FROM ({body}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0",
                d = c.d
            )
        });
        let fill = fill.collect();
        self.nodes.push(Node {
            c: c.clone(),
            body,
            inline: false,
            join: false,
            union: false,
            source: false,
            image: None,
            viewed: false,
            fill,
            delete_fill: Vec::new(),
            insert_fill: Vec::new(),
            delete_seed: Vec::new(),
            insert_seed: Vec::new(),
            ddl: Vec::new(),
            integrated: false,
            indexes: Vec::new(),
            owner,
        });
        c
    }

    fn integrate(&mut self, c: &SqlC, index: Vec<usize>) {
        self.integrate_at(c.node, index);
    }

    /// Equality probes read the index; column order is immaterial, so keys are kept sorted.
    fn integrate_at(&mut self, at: usize, mut index: Vec<usize>) {
        index.sort_unstable();
        index.dedup();
        let node = &mut self.nodes[at];
        node.integrated = true;
        if !index.is_empty() && !node.indexes.contains(&index) {
            node.indexes.push(index);
        }
    }

    /// A join term reads `c`'s image probed on `key`. A view keeps no `_i`; the key moves to the
    /// stored image under each passthrough column. Node ids fall along input edges, so the walk ends.
    fn image(&mut self, c: &SqlC, key: Vec<usize>) {
        let mut pending = vec![(c.node, key)];
        while let Some((at, key)) = pending.pop() {
            let node = &self.nodes[at];
            let parts = node.image.as_ref().filter(|_| !node.integrated).and_then(|view| {
                let mut parts: Vec<(usize, Vec<usize>)> = view.inputs.iter().map(|input| (*input, Vec::new())).collect();
                for col in &key {
                    let (input, x) = view.pass.get(*col).copied().flatten()?;
                    parts.iter_mut().find(|(node, _)| *node == input)?.1.push(x);
                }
                Some(parts)
            });
            match parts {
                Some(parts) => {
                    self.nodes[at].viewed = true;
                    pending.extend(parts);
                }
                None => self.integrate_at(at, key),
            }
        }
    }

    /// `out`'s image as `body` over `c`'s image; `pass` names the `c` column each output column
    /// copies, `extra` the stored tables `body` joins besides `c`.
    fn view(&mut self, out: &SqlC, c: &SqlC, body: String, pass: Vec<Option<usize>>, extra: usize) {
        let tables = self.view_tables(&[c.node]) + extra;
        if c.rec || self.err.is_some() || tables > VIEW_TABLES { return; }
        self.nodes[out.node].image = Some(View {
            body,
            inputs: vec![c.node],
            pass: pass.into_iter().map(|x| x.map(|x| (c.node, x))).collect(),
            tables,
        });
    }

    /// Physical images a view over `inputs` reads, counting nested views.
    fn view_tables(&self, inputs: &[usize]) -> usize {
        inputs.iter().map(|input| self.nodes[*input].image.as_ref().map_or(1, |view| view.tables)).sum()
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

    /// Join deltas read by one settle statement become its CTEs (per-round statements count
    /// twice), and only where that statement expands the CTE once: SQLite copies a CTE's select
    /// tree at every reference while it prepares, and a join delta reads each input delta twice,
    /// so a chain of CTE joins would be walked 2^depth times. A union read by more than one
    /// statement is stored: inline, every reader would hold, prepare and materialize all of its
    /// inputs. Readers have larger ids.
    fn inline_joins(&mut self) {
        let by_name: std::collections::HashMap<String, usize> =
            self.nodes.iter().map(|node| (node.c.d.clone(), node.c.node)).collect();
        let named = |sql: &str| {
            let mut found = delta_names(sql).filter_map(|name| by_name.get(name).copied()).collect::<Vec<_>>();
            found.sort_unstable();
            found.dedup();
            found
        };
        let mut reads = vec![0usize; self.nodes.len()];
        let mut statement = vec![None; self.nodes.len()];
        // References SQLite expands per node's delta, over every statement that holds it.
        let mut expansions = vec![0usize; self.nodes.len()];
        for (_, c) in &self.outputs { reads[c.node] += 1; expansions[c.node] += 1; }
        for at in (0..self.nodes.len()).rev() {
            let node = &self.nodes[at];
            if node.integrated { reads[at] += 2; expansions[at] += 2; }
            let counted_by_reader = statement[at].is_some_and(|reader: usize| {
                let fill = &self.nodes[reader].fill;
                fill.len() == 1 && fill[0].contains(FILL_ANCHOR)
            });
            if node.join && !node.inline && node.owner == Owner::Settle && !node.c.rec && !node.integrated
                && reads[at] == 1 && counted_by_reader && node.fill.len() == 1 && node.body.is_some() && node.ddl.is_empty()
                && node.delete_fill.is_empty() && node.insert_fill.is_empty()
                && node.delete_seed.is_empty() && node.insert_seed.is_empty() && expansions[at] <= 1 {
                self.nodes[at].inline = true;
            }
            let node = &self.nodes[at];
            if node.union && node.inline && node.owner == Owner::Settle && !node.c.rec && reads[at] > 1 {
                self.nodes[at].inline = false;
            }
            if self.nodes[at].source {
                self.nodes[at].inline = reads[at] <= SOURCE_READS;
            }
            let node = &self.nodes[at];
            let (own, reader) = (reads[at], if node.inline { statement[at] } else { Some(at) });
            let times = if node.inline { expansions[at] } else { 1 };
            let mut add = |sql: &str, weight: usize| {
                for input in named(sql) {
                    if input != at {
                        reads[input] += weight;
                        statement[input] = reader;
                    }
                }
                for input in delta_names(sql).filter_map(|name| by_name.get(name).copied()) {
                    if input != at { expansions[input] += times; }
                }
            };
            if node.inline {
                add(node.body.as_deref().unwrap_or(""), own);
            } else {
                let weight = if matches!(node.owner, Owner::Loop(_)) { 2 } else { 1 };
                for sql in node.fill.iter().chain(&node.delete_fill).chain(&node.insert_fill)
                    .chain(&node.delete_seed).chain(&node.insert_seed) {
                    add(sql, weight);
                }
            }
        }
    }

    /// Each `ivm_nK_i` of a viewed node without a stored image becomes its view, inputs expanded
    /// first (ascending ids), in every statement and CTE body.
    fn substitute_views(&mut self) {
        let mut views: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for node in &self.nodes {
            if let Some(view) = node.image.as_ref().filter(|_| node.viewed && !node.integrated) {
                let body = with_views(&view.body, &views);
                views.insert(node.c.i.clone(), format!("({body})"));
            }
        }
        if views.is_empty() { return; }
        for node in &mut self.nodes {
            for sql in node.body.iter_mut().chain(&mut node.fill).chain(&mut node.delete_fill)
                .chain(&mut node.insert_fill).chain(&mut node.delete_seed).chain(&mut node.insert_seed) {
                *sql = with_views(sql, &views);
            }
        }
    }

    /// Non-monotone ops over a LetRec variable are outside the round loop's DRed contract; a
    /// LetRec on the recompute path takes them.
    fn flat(&mut self, c: &SqlC, what: &'static str) {
        if c.rec && !self.active.as_ref().is_some_and(|a| a.nonmonotone) {
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
            // Term ids only grow, so the rows past the integrated maximum are the new ones.
            let c = self.push(width, false, |new| Some(format!(
                "SELECT {}, 1 AS w FROM {} t WHERE t.c0 > (SELECT coalesce(max(x_i.c0),0) FROM {} x_i)",
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
        let functor = name.replace('\'', "''");
        let ctor = crate::catalog::quote(crate::terms::ctor_table(&name));
        let old = c.arity;
        let fid = format!("(SELECT id FROM ivm_functor WHERE name='{functor}')");
        // Variant match: integer equality of each argument column against the constructor table.
        let matches = |alias: &str, src: &str| if args.is_empty() { "1".to_owned() } else {
            args.iter().enumerate().map(|(i, x)| format!("{alias}.c{} = {src}.c{x}", i + 1)).collect::<Vec<_>>().join(" AND ")
        };
        let next = self.push(old + 1, c.rec, |_| Some(format!(
            "SELECT {}, k.c0 AS c{old}, d.w FROM {} d JOIN {ctor} k ON {}",
            (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(","), c.d, matches("k", "d"),
        )));
        // New argument tuples take ids above the current ivm_term maximum, in argument order, then
        // get their ivm_term rows. Same inputs hit UNIQUE(c1..cn) and keep their id.
        let aliases = (1..=args.len()).map(|i| format!("a{i}")).collect::<Vec<_>>();
        let picked = args.iter().zip(&aliases).map(|(x, a)| format!("d.c{x} AS {a}")).chain(["1 AS k".to_owned()]).collect::<Vec<_>>().join(",");
        let order = if aliases.is_empty() { "k".to_owned() } else { aliases.join(",") };
        let ctor_cols = ["c0".to_owned()].into_iter().chain((1..=args.len()).map(|i| format!("c{i}"))).collect::<Vec<_>>().join(",");
        let ctor_vals = ["(SELECT coalesce(max(id),0) FROM ivm_term)+row_number() OVER (ORDER BY ".to_owned() + &order + ")"].into_iter().chain(aliases.iter().cloned()).collect::<Vec<_>>().join(",");
        let insert_ctor = format!(
            "INSERT INTO {ctor}({ctor_cols}) SELECT {ctor_vals} FROM (SELECT DISTINCT {picked} FROM {} d WHERE d.w>0 AND NOT EXISTS (SELECT 1 FROM {ctor} x WHERE {}))", c.d, matches("x", "d"),
        );
        let insert_term = format!(
            "{}INSERT INTO ivm_term(id,functor_id) SELECT c0, {fid} FROM {ctor} WHERE c0>(SELECT coalesce(max(id),0) FROM ivm_term)",
            crate::terms::AFTER_MINT,
        );
        self.nodes[next.node].fill.splice(0..0, [insert_ctor, insert_term]);
        Ok(next)
    }

    fn decode(&mut self, c: Self::C, functor: RelId, col: ColId, _types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
        let (name, types) = self.constructors.get(&functor)
            .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
        if col as usize >= c.arity {
            return Err(unsupported("Decode column out of range"));
        }
        let ctor = crate::catalog::quote(crate::terms::ctor_table(&name));
        let old = c.arity;
        let body = |input: &str| format!(
            "SELECT {}, {}, d.w FROM {input} d JOIN {ctor} k ON k.c0 = d.c{col}",
            (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(","),
            (0..=types.len()).map(|i| format!("k.c{i} AS c{}", old + i)).collect::<Vec<_>>().join(","),
        );
        let next = self.push(old + 1 + types.len(), c.rec, |_| Some(body(&c.d)));
        self.nodes[next.node].inline = true;
        let pass = (0..old).map(Some).chain([Some(col as usize)]).chain((0..types.len()).map(|_| None)).collect();
        self.view(&next, &c, body(&c.i), pass, 1);
        Ok(next)
    }

    fn str_cons(&mut self, c: Self::C, mode: &StrMode) -> Result<Self::C, EngineError> {
        let old = c.arity;
        let next = match mode {
            StrMode::Construct { head, rest } => {
                if *head as usize >= old || *rest as usize >= old { return Err(unsupported("StrCons column out of range")); }
                let source = format!("SELECT h.text || r.text AS text FROM {} d JOIN ivm_text h ON h.id=d.c{head} JOIN ivm_text r ON r.id=d.c{rest} WHERE d.w>0", c.d);
                let next = self.push(old + 1, c.rec, |_| Some(format!(
                    "SELECT {}, dict.id AS c{old}, d.w FROM {} d JOIN ivm_text h ON h.id=d.c{head} JOIN ivm_text r ON r.id=d.c{rest} JOIN ivm_text dict ON dict.text=h.text||r.text",
                    (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(","), c.d,
                )));
                self.nodes[next.node].fill.splice(0..0, crate::terms::mint_texts_sql(&source));
                next
            }
            StrMode::Decompose { whole } => {
                if *whole as usize >= old { return Err(unsupported("StrCons column out of range")); }
                // Head and rest texts are minted by this frontier, for every row it reads.
                let parts = ["ivm_str_head_text", "ivm_str_rest_text"].map(|part| format!(
                    "SELECT {part}(w.text) AS text FROM {} d JOIN ivm_text w ON w.id=d.c{whole} WHERE w.text<>''", c.d,
                ));
                let next = self.push(old + 2, c.rec, |_| Some(format!(
                    "SELECT {}, h.id AS c{old}, r.id AS c{}, d.w FROM {} d JOIN ivm_text w ON w.id=d.c{whole} AND w.text<>'' JOIN ivm_text h ON h.text=ivm_str_head_text(w.text) JOIN ivm_text r ON r.text=ivm_str_rest_text(w.text)",
                    (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(","), old + 1, c.d,
                )));
                self.nodes[next.node].fill.splice(0..0, crate::terms::mint_texts_sql(&parts.join(" UNION ALL ")));
                next
            }
        };
        Ok(next)
    }

    fn str_op(&mut self, c: Self::C, op: StrOp, args: &[ColId]) -> Result<Self::C, EngineError> {
        let old = c.arity;
        if args.len() != op.args().len() || args.iter().any(|col| *col as usize >= old) {
            return Err(unsupported("Str column out of range"));
        }
        let mut joins = String::new();
        let mut values = vec![format!("'{}'", op.name())];
        for (at, (col, kind)) in args.iter().zip(op.args()).enumerate() {
            match kind {
                StrKind::Text => {
                    joins.push_str(&format!(" JOIN ivm_text a{at} ON a{at}.id=d.c{col}"));
                    values.push(format!("a{at}.text"));
                }
                StrKind::Int => values.push(format!("d.c{col}")),
            }
        }
        let call = format!("ivm_str_op({})", values.join(","));
        let carried = (0..old).map(|i| format!("d.c{i} AS c{i}")).collect::<Vec<_>>().join(",");
        let texts = op.args().iter().filter(|kind| matches!(kind, StrKind::Text)).count();
        let next = match op.out() {
            None => {
                let body = |input: &str| format!("SELECT {carried}, d.w FROM {input} d{joins} WHERE {call} IS NOT NULL");
                let next = self.push(old, c.rec, |_| Some(body(&c.d)));
                self.nodes[next.node].inline = true;
                self.view(&next, &c, body(&c.i), (0..old).map(Some).collect(), texts);
                next
            }
            Some(StrKind::Text) => {
                let source = format!("SELECT {call} AS text FROM {} d{joins} WHERE d.w>0 AND {call} IS NOT NULL", c.d);
                let next = self.push(old + 1, c.rec, |_| Some(format!(
                    "SELECT {carried}, dict.id AS c{old}, d.w FROM {} d{joins} JOIN ivm_text dict ON dict.text={call}", c.d,
                )));
                self.nodes[next.node].fill.splice(0..0, crate::terms::mint_texts_sql(&source));
                next
            }
            Some(StrKind::Int) => {
                let body = |input: &str| format!("SELECT {carried}, {call} AS c{old}, d.w FROM {input} d{joins} WHERE {call} IS NOT NULL");
                let next = self.push(old + 1, c.rec, |_| Some(body(&c.d)));
                self.nodes[next.node].inline = true;
                self.view(&next, &c, body(&c.i), (0..old).map(Some).chain([None]).collect(), texts);
                next
            }
        };
        Ok(next)
    }

    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId], input_types: &[Ty]) -> Self::C {
        if filter.is_empty() && map.is_empty()
            && (project.is_empty() || (project.len() == c.arity
                && project.iter().enumerate().all(|(i, col)| *col as usize == i))) {
            return c;
        }
        let rendered = (|| -> Result<(Vec<String>, Vec<String>), EngineError> {
            let mut maps = Vec::new();
            let mut types = input_types.to_vec();
            for e in map {
                let m = render(e, c.arity, &maps, &self.texts, &types)?;
                maps.push(m);
                types.push(expr_type(e, &types).ok_or_else(|| unsupported("Mfp expression type"))?);
            }
            let wheres = filter
                .iter()
                .map(|e| Ok(format!("({}) <> 0", render(e, c.arity, &[], &self.texts, input_types)?)))
                .collect::<Result<_, _>>()?;
            let all: Vec<ColId> = (0..(c.arity + maps.len()) as ColId).collect();
            let proj = if project.is_empty() {
                &all[..]
            } else {
                project
            };
            let cols = proj
                .iter()
                .map(|p| render(&Expr::Col(*p), c.arity, &maps, &self.texts, &types))
                .collect::<Result<_, _>>()?;
            Ok((wheres, cols))
        })();
        let (wheres, cols) = rendered.unwrap_or_else(|e| {
            self.fail(e);
            (Vec::new(), vec!["0".into()])
        });
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
        let out = self.push(cols.len(), c.rec, |_| {
            Some(format!("SELECT {select}, w FROM {} WHERE {wheres}", c.d))
        });
        self.nodes[out.node].inline = true;
        let all: Vec<ColId> = (0..(c.arity + map.len()) as ColId).collect();
        let proj = if project.is_empty() { &all[..] } else { project };
        let pass = proj.iter().map(|p| {
            let p = *p as usize;
            if p < c.arity { return Some(p); }
            match map.get(p - c.arity) {
                Some(Expr::Col(x)) if (*x as usize) < c.arity => Some(*x as usize),
                _ => None,
            }
        }).collect();
        self.view(&out, &c, format!("SELECT {select}, w FROM {} WHERE {wheres}", c.i), pass, 0);
        out
    }

    fn union(&mut self, cs: Vec<Self::C>) -> Self::C {
        let cs = self.feed(cs);
        if cs.len() == 1 { return cs.into_iter().next().unwrap(); }
        let arity = cs.first().map_or(0, |c| c.arity);
        if cs.iter().any(|c| c.arity != arity) {
            self.fail(unsupported("Union arity mismatch"));
        }
        let cols = list("", 0..arity);
        let rec = cs.iter().any(|c| c.rec);
        let out = self.push(arity, rec, |_| {
            Some(
                cs.iter()
                    .map(|c| format!("SELECT {cols}, w FROM {}", c.d))
                    .collect::<Vec<_>>()
                    .join(" UNION ALL "),
            )
        });
        self.nodes[out.node].inline = true;
        self.nodes[out.node].union = true;
        out
    }

    fn negate(&mut self, c: Self::C) -> Self::C {
        self.flat(&c, "Negate over a LetRec variable");
        let out = self.push(c.arity, c.rec, |_| {
            Some(format!(
                "SELECT {}, -w AS w FROM {}",
                list("", 0..c.arity),
                c.d
            ))
        });
        self.nodes[out.node].inline = true;
        out
    }

    /// Δ(a⋈b) = Δa⋈Δb + I⁻(a)⋈Δb + Δa⋈I⁻(b), with I⁻ the integrated tables not yet updated this settle.
    fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
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
        let predicates = lk.iter().zip(&rk)
            .map(|(l, r)| equal("=", types[0][*l], format!("{{la}}.c{l}"), types[1][*r], format!("{{ra}}.c{r}")))
            .collect::<Vec<_>>();
        let shared = (a.node, b.node, predicates.clone());
        if let Some(out) = self.joins.get(&shared) {
            return Ok(out.clone());
        }
        self.image(&a, lk.clone());
        self.image(&b, rk.clone());
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
                if predicates.is_empty() { "1".to_string() } else { predicates.join(" AND ").replace("{la}", la).replace("{ra}", ra) }
            )
        };
        let terms = [
            term(&a.d, &b.d, true),
            term(&a.i, &b.d, false),
            term(&a.d, &b.i, true),
        ];
        let body = terms.join(" UNION ALL ");
        let out = self.push(a.arity + b.arity, a.rec || b.rec, |_| Some(body));
        self.nodes[out.node].join = true;
        let tables = self.view_tables(&[a.node, b.node]);
        if !a.rec && !b.rec && tables <= VIEW_TABLES {
            let (la, ra) = ("vl_i", "vr_i");
            self.nodes[out.node].image = Some(View {
                body: format!(
                    "SELECT {}, {}, {la}.w * {ra}.w AS w FROM {} {la} JOIN {} {ra} ON {}",
                    (0..a.arity).map(|i| format!("{la}.c{i} AS c{i}")).collect::<Vec<_>>().join(", "),
                    (0..b.arity).map(|i| format!("{ra}.c{i} AS c{}", a.arity + i)).collect::<Vec<_>>().join(", "),
                    a.i, b.i,
                    if predicates.is_empty() { "1".to_string() } else { predicates.join(" AND ").replace("{la}", la).replace("{ra}", ra) },
                ),
                inputs: vec![a.node, b.node],
                pass: (0..a.arity).map(|i| Some((a.node, i))).chain((0..b.arity).map(|i| Some((b.node, i)))).collect(),
                tables,
            });
        }
        if self.traced {
            let cols = list("", 0..out.arity);
            let terms = terms
                .iter()
                .map(|t| format!("SELECT {cols}, SUM(w) FROM ({t}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0 ORDER BY {cols}"))
                .collect();
            self.terms.push((out.node, terms));
        }
        self.joins.insert(shared, out.clone());
        Ok(out)
    }

    /// Recursive left inputs read the right key's phase image and seed the SCC from key changes.
    /// Other inputs use `l - l⋉threshold(π_rk r)`.
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C {
        let recompute = self.active.as_ref().is_some_and(|a| a.nonmonotone);
        if l.rec && !recompute {
            if r.rec {
                self.fail(unsupported("Antijoin recursive right input"));
                return l;
            }
            let unit = [r.arity as ColId];
            let keys = if rk.is_empty() {
                self.mfp(r, &[], &[Expr::Lit(0)], &unit, &[])
            } else {
                self.mfp(r, &[], &[], rk, &[])
            };
            let keys = self.threshold(keys);
            self.image(&keys, (0..keys.arity).collect());
            self.integrate(&l, lk.iter().map(|x| *x as usize).collect());
            let out = self.push(l.arity, true, |_| None);
            let all = list("l_d", 0..l.arity);
            let key_cols: Vec<usize> = (0..keys.arity).collect();
            let left_cols: Vec<usize> = lk.iter().map(|x| *x as usize).collect();
            let left_match = |key: &str, left: &str| {
                if lk.is_empty() { "1".into() } else { on(key, &key_cols, left, &left_cols) }
            };
            let match_old = on("k_i", &key_cols, "k", &key_cols);
            let match_left = left_match("k", "l_i");
            let absent = |deleting: bool| format!(
                "COALESCE((SELECT k_i.w FROM {ki} k_i WHERE {old_match}), 0) + \
                 COALESCE((SELECT SUM({delta}) FROM {kd} k WHERE {delta_match}), 0) <= 0",
                ki = keys.i, kd = keys.d,
                old_match = left_match("k_i", "l_d"),
                delta_match = left_match("k", "l_d"),
                delta = if deleting { "MAX(k.w, 0)" } else { "k.w" },
            );
            let node = &mut self.nodes[out.node];
            for (target, deleting) in [(&mut node.delete_fill, true), (&mut node.insert_fill, false)] {
                target.push(format!(
                    "INSERT INTO {d} SELECT {all}, l_d.w FROM {ld} l_d WHERE {absent}",
                    d = out.d, ld = l.d, absent = absent(deleting),
                ));
            }
            node.delete_seed.push(format!(
                "INSERT INTO {d} SELECT {cols}, -l_i.w FROM {kd} k CROSS JOIN {li} l_i WHERE k.w > 0 AND {match_left} AND NOT EXISTS (SELECT 1 FROM {ki} k_i WHERE {match_old} AND k_i.w > 0)",
                d = out.d, cols = list("l_i", 0..l.arity), kd = keys.d, li = l.i, ki = keys.i,
            ));
            node.insert_seed.push(format!(
                "INSERT INTO {d} SELECT {cols}, l_i.w FROM {kd} k CROSS JOIN {li} l_i WHERE k.w < 0 AND {match_left} AND NOT EXISTS (SELECT 1 FROM {ki} k_i WHERE {match_old} AND k_i.w + k.w > 0)",
                d = out.d, cols = list("l_i", 0..l.arity), kd = keys.d, li = l.i, ki = keys.i,
            ));
            return out;
        }
        let unit = [r.arity as ColId];
        let keys = if rk.is_empty() {
            self.mfp(r, &[], &[Expr::Lit(0)], &unit, &[])
        } else {
            self.mfp(r, &[], &[], rk, &[])
        };
        let keys = self.threshold(keys);
        let eq: Vec<Vec<(u8, ColId)>> = lk
            .iter()
            .enumerate()
            .map(|(i, c)| vec![(0, *c), (1, i as ColId)])
            .collect();
        let joined = match self.join(vec![l.clone(), keys], &eq, &[vec![Ty::Int; l.arity], vec![Ty::Int; rk.len()]]) {
            Ok(j) => j,
            Err(e) => {
                self.fail(e);
                return l;
            }
        };
        let left: Vec<ColId> = (0..l.arity as ColId).collect();
        let semi = self.mfp(joined, &[], &[], &left, &[]);
        let gone = self.negate(semi);
        self.union(vec![l, gone])
    }

    /// Per-group accumulators (count, sums) take Δinput by upsert; Min/Max read a private arrangement of
    /// the input by index. Touched groups emit their new row minus their stored row.
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg], input_types: &[Ty]) -> Self::C {
        self.flat(&c, "Reduce over a LetRec variable");
        let key: Vec<usize> = key.iter().map(|k| *k as usize).collect();
        if key.iter().any(|x| input_types[*x] == Ty::Any) {
            self.integrate(&c, key.clone());
        }
        let arity = key.len() + aggs.len();
        let out = self.push(arity, c.rec, |_| None);
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
            key.iter().map(|x| if input_types[*x] == Ty::Any { format!("ivm_any_key(c{x})") } else { format!("c{x}") }).collect()
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
                match a {
                    Agg::Sum(x) if input_types[*x as usize] != Ty::Any => Some((j, *x)),
                    _ => None,
                }
            })
            .collect();
        let any_sums = aggs.iter().any(|agg| matches!(agg, Agg::Sum(x) if input_types[*x as usize] == Ty::Any));
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
            sums.iter().map(|(j, x)| format!(", a{j} {} NOT NULL", if input_types[*x as usize] == Ty::Real { "REAL" } else { "INTEGER" })).collect::<String>()
        )];
        let mut fill = vec![format!(
            "INSERT INTO {g} ({gcols}, cnt{s_cols}) SELECT {}, SUM(w){} FROM {d} WHERE true {group} \
             ON CONFLICT ({gcols}) DO UPDATE SET cnt = cnt + excluded.cnt{}",
            kx.join(", "),
            sums.iter().map(|(_, x)| format!(", SUM({} * w)", if input_types[*x as usize] == Ty::Real { format!("ivm_real_value(c{x})") } else { format!("c{x}") })).collect::<String>(),
            sums.iter().map(|(j, _)| format!(", a{j} = a{j} + excluded.a{j}")).collect::<String>(),
            d = c.d
        )];
        let all = list("", 0..c.arity);
        if !extremes.is_empty() || any_sums {
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
                .map(|(x, t)| if input_types[*x] == Ty::Any { format!("ivm_any_key(n_i.c{x}) = {alias}.g{t}") } else { format!("n_i.c{x} = {alias}.g{t}") })
                .collect();
            if pairs.is_empty() {
                "1".to_string()
            } else {
                pairs.join(" AND ")
            }
        };
        let values = aggs.iter().enumerate().map(|(j, agg)| match agg {
            Agg::Count => "g_i.cnt".to_string(),
            Agg::Sum(x) if input_types[*x as usize] == Ty::Any => format!("(SELECT ivm_any_id(SUM(v)) FROM (WITH RECURSIVE expanded(v,n) AS (SELECT ivm_any_value(n_i.c{x}), n_i.w FROM {arr} n_i WHERE {} AND n_i.w>0 UNION ALL SELECT v,n-1 FROM expanded WHERE n>1) SELECT v FROM expanded))", g_on("g_i")),
            Agg::Sum(x) => if input_types[*x as usize] == Ty::Real { format!("ivm_real_bits(g_i.a{j})") } else { format!("g_i.a{j}") },
            Agg::Min(x) | Agg::Max(x) => {
                let is_any = input_types[*x as usize] == Ty::Any;
                let filter = if is_any { format!(" AND ivm_any_value(n_i.c{x}) IS NOT NULL") } else { String::new() };
                let direction = if matches!(agg, Agg::Max(_)) { " DESC" } else { "" };
                let query = format!("(SELECT n_i.c{x} FROM {arr} n_i WHERE {} AND n_i.w > 0{filter} ORDER BY {}{direction} LIMIT 1)",
                    g_on("g_i"), ordered(input_types[*x as usize], format!("n_i.c{x}")));
                if is_any { format!("COALESCE({query},ivm_any_id(NULL))") } else { query }
            }
        });
        let select = key
            .iter()
            .zip(&gk)
            .map(|(col, group)| if input_types[*col] == Ty::Any {
                let candidate = list("", 0..c.arity);
                let cond = key.iter().zip(&gk).map(|(x, gcol)| {
                    if input_types[*x] == Ty::Any { format!("ivm_any_key(v.c{x}) = g_i.g{gcol}") }
                    else { format!("v.c{x} = g_i.g{gcol}") }
                }).collect::<Vec<_>>().join(" AND ");
                format!("(SELECT v.c{col} FROM (SELECT {candidate}, SUM(w) AS w FROM (SELECT {candidate}, w FROM {} UNION ALL SELECT {candidate}, w FROM {}) GROUP BY {candidate}) v WHERE v.w>0 AND {cond} ORDER BY {} LIMIT 1)", c.i, c.d, key.iter().map(|x| format!("v.c{x}")).collect::<Vec<_>>().join(", "))
            } else { format!("g_i.g{group}") })
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
        let out = self.push(c.arity, c.rec, |_| {
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
        });
        self.nodes[out.node].inline = true;
        // The input image is stored and consolidated: one row per value.
        self.nodes[out.node].image = Some(View {
            body: format!("SELECT {}, 1 AS w FROM {} WHERE w > 0", list("", 0..c.arity), c.i),
            inputs: vec![c.node],
            pass: n.iter().map(|x| Some((c.node, *x))).collect(),
            tables: 1,
        });
        out
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
                    let value = ordered(input_types[x], format!("c{x}"));
                    format!("{value}{}", if o.desc { " DESC" } else { "" })
                })
                .chain((0..c.arity).map(|x| ordered(input_types[x], format!("c{x}"))))
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
            let value = ordered(input_types[x], format!("c{x}"));
            format!("{value}{}", if o.desc { " DESC" } else { " ASC" })
        }).collect::<Vec<_>>();
        let by = order_cols.iter().cloned()
            .chain((0..c.arity).map(|x| format!("{} ASC", ordered(input_types[x], format!("c{x}")))))
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
        _outer: &mut Vec<Option<Self::C>>,
    ) -> Result<Vec<Self::C>, EngineError> {
        if self.active.is_some() {
            return Err(unsupported("LetRec nested in LetRec"));
        }
        if rec.limit == Some(0) {
            return Err(unsupported("LetRec limit 0"));
        }
        let scc = self.sccs.len();
        let nonmonotone = nonmonotone(p, rec);
        self.active = Some(Active {
            scc,
            copies: Vec::new(),
            nonmonotone,
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
            // DRed reads and clears the body delta after each round.
            self.nodes[b.node].inline = false;
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
        self.sccs.push(SccPlan { vars, at, nonmonotone, limit: rec.limit, first: rec.ids.first().copied() });
        self.mark = self.nodes.len();
        Ok(rs)
    }

    fn output(&mut self, rel: RelId, c: Self::C) {
        self.integrate(&c, Vec::new());
        self.outputs.push((rel, c));
    }

    fn node_types(&mut self, p: &Program, id: NodeId) -> Option<Vec<Ty>> {
        p.node_types_memo(id, &mut self.types)
    }

    /// The returned SQL node is the IR node's own delta; SQL nodes pushed since the previous observe are its internals.
    fn observe(&mut self, id: NodeId, c: Self::C) -> Self::C {
        self.work_nodes.push((id, c.node));
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
    /// Recompute path: the old value into `acc` at -1, before `reset` empties the variable.
    capture: String,
    /// Recompute path: the variable's change this round, from the body rows that changed:
    /// +1 where the body holds a row the variable lacks, -1 the other way.
    step: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Delete, Insert, Recompute }

#[derive(Default)]
struct SccSql {
    delete_fills: Vec<String>,
    insert_fills: Vec<String>,
    delete_seeds: Vec<String>,
    insert_seeds: Vec<String>,
    integrates: Vec<String>,
    clears: Vec<String>,
    vars: Vec<VarSql>,
    /// Per copy: move positive rows aside, then later move them back.
    stash: Vec<String>,
    restore: Vec<String>,
    /// Physical deltas each phase fill reads, and the one it writes.
    delete_io: Vec<(Vec<usize>, Option<usize>)>,
    insert_io: Vec<(Vec<usize>, Option<usize>)>,
    integrate_reads: Vec<Vec<usize>>,
    clear_targets: Vec<Option<usize>>,
    delete_seed_writes: Vec<Option<usize>>,
    insert_seed_writes: Vec<Option<usize>>,
    /// Physical delta of each variable, in `vars` order.
    var_ids: Vec<usize>,
    nonmonotone: bool,
    limit: Option<u32>,
    first: Option<RelId>,
    /// Recompute path: empties every loop-owned image and private table, and turns each copy's
    /// delta into its input's whole post-frontier image.
    reset: Vec<String>,
    /// Physical id of each copy, marked changed for the first recompute round.
    copy_ids: Vec<usize>,
}

pub(crate) struct NodesPlan {
    pub ddl: Vec<String>,
    pub objects: Vec<(String, String)>,
    /// Each source's delta CTE name; the stage holds its rows.
    source_deltas: Vec<String>,
    source_ids: Vec<Option<usize>>,
    /// Source relation of each `source_deltas` entry.
    source_tables: Vec<String>,
    /// Distinct relations in the stage.
    staged_sql: String,
    /// Physical nodes and sources: the index space of `active`.
    node_count: usize,
    steps: Vec<Step>,
    step_reads: Vec<Vec<usize>>,
    step_writes: Vec<Option<usize>>,
    scc_reads: Vec<Vec<usize>>,
    scc_results: Vec<Vec<usize>>,
    sccs: Vec<SccSql>,
    integrates: Vec<String>,
    integrate_reads: Vec<Vec<usize>>,
    clears: Vec<String>,
    clear_targets: Vec<Option<usize>>,
    work_reads: Vec<Vec<usize>>,
    pub output_delta: String,
    pub output_snapshot: String,
    /// `output_snapshot` restricted to rows whose first column is `?1`.
    pub member_snapshot: String,
    pub output_arity: usize,
    work: Vec<(Owner, String, WorkKind, usize)>,
    /// Per `work` entry, the slot of an inline join in `work_rows`.
    work_slots: Vec<Option<u64>>,
    /// Rows of each inline join delta this settle pass, written by `work_function` as the one
    /// statement that reads the delta materializes it.
    work_rows: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    work_function: String,
    /// `count_work`'s statement per `work` entry without a slot, built once after `prefix`.
    work_sql: Vec<String>,
    filter_measured: bool,
    mint_measured: bool,
}

/// Every whole `ivm_n...` identifier in `sql`, with its byte offset.
fn node_names(sql: &str) -> impl Iterator<Item = (usize, &str)> {
    let bytes = sql.as_bytes();
    sql.match_indices("ivm_n").filter_map(move |(at, _)| {
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
            return None;
        }
        let end = bytes[at..].iter().position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .map(|len| at + len).unwrap_or(bytes.len());
        Some((at, &sql[at..end]))
    })
}

fn delta_names(sql: &str) -> impl Iterator<Item = &str> {
    node_names(sql).map(|(_, name)| name).filter(|name| name.ends_with("_d"))
}

fn with_views(sql: &str, views: &std::collections::HashMap<String, String>) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for (at, name) in node_names(sql) {
        if let Some(view) = views.get(name) {
            out.push_str(&sql[last..at]);
            out.push_str(view);
            last = at + name.len();
        }
    }
    out.push_str(&sql[last..]);
    out
}

fn delta_refs(sql: &str, by_name: &std::collections::HashMap<&str, usize>) -> Vec<usize> {
    let bytes = sql.as_bytes();
    sql.match_indices("ivm_n").filter_map(|(at, _)| {
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
            return None;
        }
        let end = bytes[at..].iter().position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .map(|len| at + len).unwrap_or(bytes.len());
        by_name.get(&sql[at..end]).copied()
    }).collect()
}

/// `program`'s SQL function `(slot, rows)` that records an inline join's delta row count.
fn work_function(program: &str) -> String {
    format!("frontier_{program}_work")
}

/// The fill anchor of the one statement that reads an inline join (`push`'s fill shape).
const FILL_ANCHOR: &str = ", SUM(w) FROM (";

fn with_inline_deltas(sql: &str, definitions: &[(String, String)],
    by_name: &std::collections::HashMap<&str, usize>, deps: &[Vec<usize>],
    counted: &std::collections::HashMap<String, String>) -> String {
    let mut needed = vec![false; definitions.len()];
    let mut pending = delta_refs(sql, by_name);
    while let Some(i) = pending.pop() {
        if !needed[i] {
            needed[i] = true;
            pending.extend(deps[i].iter().copied());
        }
    }
    // A one-row aggregate leads the fill's FROM: it runs once per execution, before the body, so
    // each counted CTE is materialized and counted even when the body reads none of its rows.
    let counts = definitions.iter().zip(&needed).filter(|(_, needed)| **needed)
        .filter_map(|((name, _), _)| counted.get(name).map(|call| format!("(SELECT {call} FROM {name}) CROSS JOIN ")))
        .collect::<String>();
    let sql = match sql.find(FILL_ANCHOR) {
        Some(at) if !counts.is_empty() => format!("{}, SUM(w) FROM {counts}({}", &sql[..at], &sql[at + FILL_ANCHOR.len()..]),
        _ => sql.to_owned(),
    };
    let ctes = definitions.iter().zip(needed).filter_map(|((name, body), needed)| {
        needed.then(|| format!("{name} AS MATERIALIZED ({body})"))
    }).collect::<Vec<_>>().join(", ");
    if ctes.is_empty() { return sql; }
    if let Some(rest) = sql.strip_prefix("WITH RECURSIVE ") {
        format!("WITH RECURSIVE {ctes}, {rest}")
    } else if let Some(rest) = sql.strip_prefix("WITH ") {
        format!("WITH {ctes}, {rest}")
    } else {
        format!("WITH {ctes} {sql}")
    }
}

impl NodesPlan {
    /// Statements issued during settle, excluding snapshot reads.
    pub fn statements(&self) -> Vec<&str> {
        let mut all: Vec<&str> = Vec::new();
        all.extend(self.steps.iter().filter_map(|step| match step {
            Step::Fill(sql) => Some(sql.as_str()),
            Step::Loop(_) => None,
        }));
        for scc in &self.sccs {
            all.extend(scc.delete_fills.iter().chain(&scc.insert_fills)
                .chain(&scc.delete_seeds).chain(&scc.insert_seeds)
                .chain(&scc.integrates).chain(&scc.clears).chain(&scc.stash).chain(&scc.restore)
                .chain(&scc.reset).map(String::as_str));
            for v in &scc.vars {
                all.extend([
                    &v.over_delete, &v.rederive, &v.insert, &v.to_delta, &v.to_acc,
                    &v.to_del, &v.clear_nx, &v.clear_del, &v.any, &v.finish, &v.clear_acc,
                ].into_iter().map(String::as_str));
                if scc.nonmonotone { all.extend([v.capture.as_str(), v.step.as_str()]); }
            }
        }
        all.extend(self.integrates.iter().chain(&self.clears).map(String::as_str));
        all.push(&self.output_delta);
        all
    }

    pub fn compile(name: &str, program: &Program, set_output: bool) -> Result<Self, EngineError> {
        let mut rel = SqlRel::new(program, false);
        lower(program, &mut rel)?;
        if let Some(error) = rel.err {
            return Err(error);
        }
        let stage = crate::catalog::quote(crate::catalog::stage(name));
        for (id, c) in &rel.sources {
            let source = program.rel(*id).ok_or_else(|| {
                EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id))
            })?;
            let node = &mut rel.nodes[c.node];
            node.body = Some(format!("SELECT {}, __sign AS w FROM {stage} WHERE __table='{}'",
                (0..c.arity).map(|i| format!("v{i} AS c{i}")).collect::<Vec<_>>().join(", "),
                source.name.replace('\'', "''")));
            node.inline = true;
            let cols = list("", 0..c.arity);
            node.fill = vec![format!("INSERT INTO {} ({cols}, w) SELECT {cols}, SUM(w) FROM ({}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0",
                c.d, node.body.as_deref().unwrap_or(""))];
        }
        rel.inline_joins();
        rel.substitute_views();
        let filter_measured = !rel.work_nodes.iter().any(|(id, node)| {
            matches!(program.nodes.get(*id as usize), Some(Op::Mfp { .. }))
                && rel.nodes.get(*node).is_some_and(|built| built.inline)
        });
        let mint_measured = !rel.work_nodes.iter().any(|(id, node)| {
            matches!(program.nodes.get(*id as usize), Some(Op::Mint { .. } | Op::StrCons { .. } | Op::Str { .. }))
                && rel.nodes.get(*node).is_some_and(|built| built.inline)
        });
        // An inline join's delta rows are counted as its one reading statement materializes them.
        let slots: std::collections::HashMap<usize, u64> = rel.nodes.iter().filter(|node| node.inline && node.join)
            .enumerate().map(|(slot, node)| (node.c.node, slot as u64)).collect();
        let (work, work_slots): (Vec<_>, Vec<_>) = rel.work_nodes.iter().filter_map(|(id, node)| {
            let kind = WorkKind::of(program.nodes.get(*id as usize)?)?;
            let built = rel.nodes.get(*node)?;
            if (built.inline && !built.join) || (kind == WorkKind::Filter && !filter_measured)
                || (kind == WorkKind::Mint && !mint_measured) { return None; }
            Some(((built.owner, built.c.d.clone(), kind, built.c.arity), slots.get(node).copied()))
        }).unzip();
        let inline_deltas = rel.nodes.iter().filter(|node| node.inline).map(|node| {
            let cols = list("", 0..node.c.arity);
            let body = node.body.as_ref().expect("inline node has a body");
            (node.c.d.clone(), format!(
                "SELECT {cols}, SUM(w) AS w FROM ({body}) WHERE true GROUP BY {cols} HAVING SUM(w) <> 0"
            ))
        }).collect::<Vec<_>>();
        let mut ddl = Vec::new();
        let mut objects = Vec::new();
        let mut source_deltas = Vec::new();
        let mut source_tables = Vec::new();
        let (mut steps, mut integrates, mut clears) = (Vec::new(), Vec::new(), Vec::new());
        let mut sccs: Vec<SccSql> = rel.sccs.iter().map(|_| SccSql::default()).collect();
        let mut hold_width = vec![0usize; rel.sccs.len()];
        for node in &rel.nodes {
            if let Owner::Copy(s) = node.owner { hold_width[s] = hold_width[s].max(node.c.arity); }
        }
        for node in &rel.nodes {
            let SqlC {
                node: k,
                d,
                i,
                arity,
                ..
            } = &node.c;
            let cols = list("", 0..*arity);
            if !node.inline {
                ddl.push(format!("CREATE TABLE {d} ({}, w INTEGER NOT NULL)", decl(*arity)));
                objects.push(("TABLE".into(), d.clone()));
            }
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
                Owner::Loop(s) => {
                    if !node.inline {
                        sccs[s].delete_fills.extend(node.fill.iter().cloned());
                        sccs[s].insert_fills.extend(node.fill.iter().cloned());
                    }
                    sccs[s].delete_fills.extend(node.delete_fill.iter().cloned());
                    sccs[s].insert_fills.extend(node.insert_fill.iter().cloned());
                    sccs[s].delete_seeds.extend(node.delete_seed.iter().cloned());
                    sccs[s].insert_seeds.extend(node.insert_seed.iter().cloned());
                }
                _ if !node.inline => steps.extend(node.fill.iter().cloned().map(Step::Fill)),
                _ => {}
            }
            if let Owner::Copy(s) = node.owner {
                // One hold table per SCC: `k` names the copy, columns past its arity hold 0.
                let hold = format!("ivm_n{}_hold", rel.sccs[s].at);
                if sccs[s].stash.is_empty() {
                    ddl.push(format!(
                        "CREATE TABLE {hold} (k INTEGER NOT NULL, {}, w INTEGER NOT NULL)",
                        decl(hold_width[s])
                    ));
                    objects.push(("TABLE".into(), hold.clone()));
                }
                let pad = (*arity..hold_width[s]).map(|_| ", 0").collect::<String>();
                sccs[s].stash.extend([
                    format!("INSERT INTO {hold} SELECT {k}, {cols}{pad}, w FROM {d} WHERE w > 0"),
                    format!("DELETE FROM {d} WHERE w > 0"),
                ]);
                sccs[s].restore.push(format!("INSERT INTO {d} SELECT {cols}, w FROM {hold} WHERE k = {k}"));
            }
            match node.owner {
                Owner::Loop(s) if rel.sccs[s].nonmonotone => {
                    if node.integrated { sccs[s].reset.push(format!("DELETE FROM {i}")); }
                    if !node.inline { sccs[s].reset.push(format!("DELETE FROM {d}")); }
                    for extra in &node.ddl {
                        if let Some((kind, name)) = ddl_object(extra) {
                            if kind == "TABLE" { sccs[s].reset.push(format!("DELETE FROM {name}")); }
                        }
                    }
                }
                Owner::Copy(s) if rel.sccs[s].nonmonotone => sccs[s].reset.extend([
                    format!("INSERT INTO {i} ({cols}, w) SELECT {cols}, SUM(w) FROM {d} WHERE true GROUP BY {cols} \
                        ON CONFLICT ({cols}) DO UPDATE SET w = w + excluded.w"),
                    format!("DELETE FROM {d}"),
                    format!("INSERT INTO {d} ({cols}, w) SELECT {cols}, w FROM {i} WHERE w <> 0"),
                    format!("DELETE FROM {i}"),
                ]),
                _ => {}
            }
            let (ints, clrs) = match node.owner {
                Owner::Settle => (&mut integrates, &mut clears),
                Owner::Loop(s) | Owner::Copy(s) => {
                    let scc = &mut sccs[s];
                    (&mut scc.integrates, &mut scc.clears)
                }
            };
            if !node.inline { clrs.push(format!("DELETE FROM {d}")); }
            if node.integrated {
                ddl.push(format!(
                    "CREATE TABLE {i} ({}, w INTEGER NOT NULL, PRIMARY KEY ({cols})) WITHOUT ROWID",
                    decl(*arity)
                ));
                objects.push(("TABLE".into(), i.clone()));
                let pk: Vec<usize> = (0..*arity).collect();
                let served = |index: &Vec<usize>| pk.starts_with(index)
                    || node.indexes.iter().any(|other| other.len() > index.len() && other.starts_with(index));
                for (j, index) in node.indexes.iter().enumerate().filter(|(_, index)| !served(index)) {
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
            (scc.nonmonotone, scc.limit, scc.first) = (plan.nonmonotone, plan.limit, plan.first);
            if !scc.restore.is_empty() {
                scc.restore.push(format!("DELETE FROM ivm_n{}_hold", plan.at));
            }
            // One `nx`, `del` and `acc` per SCC: `k` names the variable, columns past its arity hold 0.
            let width = plan.vars.iter().map(|(v, _, _)| v.arity).max().unwrap_or(0);
            let (nx, del, acc) = (
                format!("ivm_n{}_nx", plan.at),
                format!("ivm_n{}_del", plan.at),
                format!("ivm_n{}_acc", plan.at),
            );
            if !plan.vars.is_empty() {
                for t in [&nx, &del, &acc] {
                    ddl.push(format!(
                        "CREATE TABLE {t} (k INTEGER NOT NULL, {}, w INTEGER NOT NULL)",
                        decl(width)
                    ));
                    objects.push(("TABLE".into(), t.clone()));
                }
            }
            for (k, (v, b, r)) in plan.vars.iter().enumerate() {
                let n = v.arity;
                let (cols, all) = (list("", 0..n), (0..n).collect::<Vec<_>>());
                let pad = (n..width).map(|_| ", 0").collect::<String>();
                let live = |alias: &str, table: &str| {
                    format!(
                        "EXISTS (SELECT 1 FROM {table} x_i WHERE {} AND x_i.w > 0)",
                        on("x_i", &all, alias, &all)
                    )
                };
                scc.vars.push(VarSql {
                    over_delete: format!("INSERT INTO {nx} SELECT {k}, {cols}{pad}, -1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w < 0) t WHERE {}", live("t", &v.i), bd=b.d),
                    rederive: format!("INSERT INTO {nx} SELECT {k}, {cols}{pad}, 1 FROM {del} t WHERE t.k = {k} AND {}", live("t", &b.i)),
                    insert: format!("INSERT INTO {nx} SELECT {k}, {cols}{pad}, 1 FROM (SELECT DISTINCT {cols} FROM {bd} WHERE w > 0) t WHERE {} AND NOT {}", live("t", &b.i), live("t", &v.i), bd=b.d),
                    to_delta: format!("INSERT INTO {} SELECT {cols}, w FROM {nx} WHERE k = {k}", v.d),
                    to_acc: format!("INSERT INTO {acc} SELECT * FROM {nx} WHERE k = {k}"),
                    to_del: format!("INSERT INTO {del} SELECT * FROM {nx} WHERE k = {k}"),
                    clear_nx: format!("DELETE FROM {nx} WHERE k = {k}"),
                    clear_del: format!("DELETE FROM {del} WHERE k = {k}"),
                    any: format!("SELECT EXISTS (SELECT 1 FROM {})", v.d),
                    finish: format!("INSERT INTO {} SELECT {cols}, SUM(w) FROM {acc} WHERE k = {k} GROUP BY {cols} HAVING SUM(w) <> 0", r.d),
                    clear_acc: format!("DELETE FROM {acc} WHERE k = {k}"),
                    capture: format!("INSERT INTO {acc} SELECT {k}, {cols}{pad}, -1 FROM {} WHERE w > 0", v.i),
                    step: format!(
                        "INSERT INTO {nx} SELECT {k}, {cols}{pad}, CASE WHEN {body} THEN 1 ELSE -1 END \
                         FROM (SELECT DISTINCT {cols} FROM {bd}) t WHERE {body} <> {var}",
                        body = live("t", &b.i), var = live("t", &v.i), bd = b.d,
                    ),
                });
            }
        }
        for (id, c) in &rel.sources {
            let source = program.rel(*id).ok_or_else(|| {
                EngineError::new(Stage::Install, Some(*id), ErrorKind::UnknownRel(*id))
            })?;
            source_deltas.push(c.d.clone());
            source_tables.push(source.name.clone());
        }
        // A pure delta can read an input's pre-frontier image. Integrate consumers
        // before inputs so those images remain the pre-frontier values throughout.
        integrates = integrates.chunks_exact(2).rev().flatten().cloned().collect();
        for scc in &mut sccs {
            scc.integrates = scc.integrates.chunks_exact(2).rev().flatten().cloned().collect();
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
            source_deltas,
            source_ids: Vec::new(),
            source_tables,
            staged_sql: format!("SELECT DISTINCT __table FROM {}", crate::catalog::quote(crate::catalog::stage(name))),
            node_count: 0,
            steps,
            step_reads: Vec::new(),
            step_writes: Vec::new(),
            scc_reads: Vec::new(),
            scc_results: Vec::new(),
            sccs,
            integrates,
            integrate_reads: Vec::new(),
            clears,
            clear_targets: Vec::new(),
            work_reads: Vec::new(),
            output_delta: if set_output { format!(
                "SELECT {out_cols}, CASE WHEN COALESCE(i_i.w,0)>0 THEN -1 ELSE 1 END \
                 FROM (SELECT {cols},SUM(w) AS dw FROM {delta} GROUP BY {cols} HAVING SUM(w)<>0) d \
                 LEFT JOIN {integrated} i_i ON {same} \
                 WHERE (COALESCE(i_i.w,0)>0) <> (COALESCE(i_i.w,0)+d.dw>0) ORDER BY {out_cols}",
                out_cols = list("d", 0..output.arity),
                delta = output.d,
                integrated = output.i,
                same = on("d", &(0..output.arity).collect::<Vec<_>>(), "i_i", &(0..output.arity).collect::<Vec<_>>()),
            ) } else { format!(
                "SELECT {cols},SUM(w) FROM {} GROUP BY {cols} HAVING SUM(w)<>0 ORDER BY {cols}",
                output.d
            ) },
            output_snapshot: format!(
                "SELECT {cols},w FROM {} WHERE w>0 ORDER BY {cols}",
                output.i
            ),
            member_snapshot: format!(
                "SELECT {cols},w FROM {} WHERE c0=?1 AND w>0 ORDER BY {cols}",
                output.i
            ),
            output_arity: output.arity,
            work,
            work_slots,
            work_rows: std::sync::Arc::new(std::sync::Mutex::new(vec![0; slots.len()])),
            work_function: work_function(name),
            work_sql: Vec::new(),
            filter_measured,
            mint_measured,
        };
        plan.work_sql = plan.work.iter().zip(&plan.work_slots).map(|((_, table, _, arity), slot)| {
            let cols = list("", 0..*arity);
            if slot.is_some() { return String::new(); }
            format!("SELECT count(*) FROM (SELECT {cols} FROM {table} GROUP BY {cols} HAVING sum(w)<>0)")
        }).collect();
        let counted = slots.iter().map(|(node, slot)| (rel.nodes[*node].c.d.clone(), format!("{}({slot}, count(*))", work_function(name)))).collect();
        plan.inline_statements(&inline_deltas, &counted);
        plan.prefix(name);
        plan.index_delta_dependencies(&rel.nodes, name);
        Ok(plan)
    }

    fn index_delta_dependencies(&mut self, nodes: &[Node], program: &str) {
        let names = nodes.iter().filter(|node| !node.inline || node.source)
            .map(|node| node.c.d.replace("ivm_n", &format!("frontier_{program}_n")))
            .collect::<Vec<_>>();
        let by_name = names.iter().enumerate().map(|(i, name)| (name.as_str(), i))
            .collect::<std::collections::HashMap<_, _>>();
        let write = |sql: &str| sql.rsplit_once("INSERT INTO ")
            .and_then(|(_, tail)| tail.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next())
            .and_then(|name| by_name.get(name).copied());
        let reads = |sql: &str| {
            let prefix = format!("frontier_{program}_n");
            let bytes = sql.as_bytes();
            let mut found = sql.match_indices(&prefix).filter_map(|(at, _)| {
                if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
                    return None;
                }
                if sql[..at].ends_with("INSERT INTO ") { return None; }
                let end = bytes[at..].iter().position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
                    .map(|len| at + len).unwrap_or(bytes.len());
                by_name.get(&sql[at..end]).copied()
            }).collect::<Vec<_>>();
            found.sort_unstable();
            found.dedup();
            found
        };
        self.source_ids = self.source_deltas.iter().map(|name| by_name.get(name.as_str()).copied()).collect();
        self.integrate_reads = self.integrates.iter().map(|sql| reads(sql)).collect();
        self.clear_targets = self.clears.iter().map(|sql| sql.strip_prefix("DELETE FROM ")
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|name| by_name.get(name).copied())).collect();
        self.work_reads = self.work_sql.iter().map(|sql| reads(sql)).collect();
        for step in &self.steps {
            match step {
                Step::Fill(sql) => {
                    self.step_reads.push(reads(sql));
                    self.step_writes.push(write(sql));
                }
                Step::Loop(_) => {
                    self.step_reads.push(Vec::new());
                    self.step_writes.push(None);
                }
            }
        }
        self.node_count = names.len();
        let target = |sql: &str| sql.strip_prefix("DELETE FROM ")
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|name| by_name.get(name).copied());
        for scc in &mut self.sccs {
            scc.delete_io = scc.delete_fills.iter().map(|sql| (reads(sql), write(sql))).collect();
            scc.insert_io = scc.insert_fills.iter().map(|sql| (reads(sql), write(sql))).collect();
            scc.integrate_reads = scc.integrates.iter().map(|sql| reads(sql)).collect();
            scc.clear_targets = scc.clears.iter().map(|sql| target(sql)).collect();
            scc.delete_seed_writes = scc.delete_seeds.iter().map(|sql| write(sql)).collect();
            scc.insert_seed_writes = scc.insert_seeds.iter().map(|sql| write(sql)).collect();
            scc.var_ids = scc.vars.iter().map(|v| write(&v.to_delta).expect("a variable's delta is physical")).collect();
        }
        for (scc_id, scc) in self.sccs.iter_mut().enumerate() {
            scc.copy_ids = nodes.iter().filter(|node| node.owner == Owner::Copy(scc_id))
                .filter_map(|node| by_name.get(node.c.d.replace("ivm_n", &format!("frontier_{program}_n")).as_str()).copied())
                .collect();
        }
        for (scc_id, scc) in self.sccs.iter().enumerate() {
            let internal = nodes.iter().filter(|node| node.owner == Owner::Loop(scc_id))
                .filter_map(|node| by_name.get(node.c.d.replace("ivm_n", &format!("frontier_{program}_n")).as_str()).copied())
                .collect::<Vec<_>>();
            let sqls = scc.delete_fills.iter().chain(&scc.insert_fills)
                .chain(&scc.delete_seeds).chain(&scc.insert_seeds)
                .chain(&scc.integrates).chain(&scc.stash).chain(&scc.restore);
            let mut external = sqls.flat_map(|sql| reads(sql)).collect::<Vec<_>>();
            external.retain(|id| !internal.contains(id));
            external.sort_unstable();
            external.dedup();
            self.scc_reads.push(external);
            self.scc_results.push(scc.vars.iter().map(|v| write(&v.finish)
                .expect("SCC finish writes a physical delta")).collect());
        }
    }

    fn inline_statements(&mut self, inline_deltas: &[(String, String)], counted: &std::collections::HashMap<String, String>) {
        let by_name = inline_deltas.iter().enumerate().map(|(i, (name, _))| (name.as_str(), i))
            .collect::<std::collections::HashMap<_, _>>();
        let deps = inline_deltas.iter().map(|(_, body)| delta_refs(body, &by_name)).collect::<Vec<_>>();
        let expand = |sql: &mut String| *sql = with_inline_deltas(sql, inline_deltas, &by_name, &deps, counted);

        for step in &mut self.steps {
            if let Step::Fill(sql) = step { expand(sql); }
        }
        for scc in &mut self.sccs {
            scc.delete_fills.iter_mut().chain(&mut scc.insert_fills)
                .chain(&mut scc.delete_seeds).chain(&mut scc.insert_seeds)
                .chain(&mut scc.integrates).chain(&mut scc.clears)
                .chain(&mut scc.stash).chain(&mut scc.restore).chain(&mut scc.reset).for_each(&expand);
            for var in &mut scc.vars {
                for sql in [&mut var.over_delete, &mut var.rederive, &mut var.insert,
                    &mut var.to_delta, &mut var.to_acc, &mut var.to_del, &mut var.clear_nx,
                    &mut var.clear_del, &mut var.any, &mut var.finish, &mut var.clear_acc,
                    &mut var.capture, &mut var.step] {
                    expand(sql);
                }
            }
        }
        self.integrates.iter_mut().chain(&mut self.clears).chain(&mut self.work_sql).for_each(&expand);
        expand(&mut self.output_delta);
    }

    fn prefix(&mut self, program: &str) {
        let prefix = format!("frontier_{program}_n");
        let fix = |s: &mut String| *s = s.replace("ivm_n", &prefix);
        self.ddl.iter_mut().for_each(&fix);
        for (_, name) in &mut self.objects {
            fix(name);
        }
        self.source_deltas.iter_mut().for_each(&fix);
        for step in &mut self.steps {
            if let Step::Fill(sql) = step {
                fix(sql);
            }
        }
        for scc in &mut self.sccs {
            scc.delete_fills.iter_mut().for_each(&fix);
            scc.insert_fills.iter_mut().for_each(&fix);
            scc.delete_seeds.iter_mut().for_each(&fix);
            scc.insert_seeds.iter_mut().for_each(&fix);
            scc.integrates.iter_mut().for_each(&fix);
            scc.clears.iter_mut().for_each(&fix);
            scc.stash.iter_mut().for_each(&fix);
            scc.restore.iter_mut().for_each(&fix);
            scc.reset.iter_mut().for_each(&fix);
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
                    &mut var.capture,
                    &mut var.step,
                ] {
                    fix(sql);
                }
            }
        }
        self.integrates.iter_mut().for_each(&fix);
        self.clears.iter_mut().for_each(&fix);
        fix(&mut self.output_delta);
        fix(&mut self.output_snapshot);
        fix(&mut self.member_snapshot);
        for (_, table, _, _) in &mut self.work { fix(table); }
        self.work_sql.iter_mut().for_each(&fix);
    }

    /// One settle statement under its own `stmt` span (object: the table it writes).
    fn exec(db: &Connection, sql: &str, counters: &mut Counters) -> rusqlite::Result<usize> {
        let result = sqlite_ext::statements::exec_cached(db, "settle", object_of(sql), sql, [])?;
        *counters.statements.as_mut().unwrap() += 1;
        Ok(result)
    }

    /// A registration statement after a mint runs only when that mint inserted rows: the
    /// constructor or text rows it registers are the ones inserted above the stored maximum.
    fn exec_all<'a>(
        db: &Connection,
        sqls: impl IntoIterator<Item = &'a String>,
        counters: &mut Counters,
    ) -> rusqlite::Result<()> {
        let mut minted = false;
        for sql in sqls {
            if sql.starts_with(crate::terms::AFTER_MINT) {
                if minted { Self::exec(db, sql, counters)?; }
                continue;
            }
            minted = Self::exec(db, sql, counters)? > 0;
        }
        Ok(())
    }

    fn count_work(&self, db: &Connection, owner: Owner, active: Option<&[bool]>, counters: &mut Counters) -> rusqlite::Result<()> {
        for (i, (at, _, kind, _)) in self.work.iter().enumerate() {
            let matches = match (*at, owner) {
                (Owner::Settle | Owner::Copy(_), Owner::Settle) => true,
                (Owner::Loop(a), Owner::Loop(b)) => a == b,
                _ => false,
            };
            if let (true, Some(slot)) = (matches, self.work_slots[i]) {
                let rows = self.work_rows.lock().map_err(|_| rusqlite::Error::InvalidQuery)?[slot as usize];
                kind.add(counters, rows);
                continue;
            }
            if matches && active.is_none_or(|active| self.work_reads[i].is_empty() || self.work_reads[i].iter().any(|id| active.get(*id) == Some(&true))) {
                let sql = &self.work_sql[i];
                let rows: i64 = db.prepare_cached(sql)?.query_row([], |r| r.get(0))?;
                *counters.statements.as_mut().unwrap() += 1;
                kind.add(counters, rows as u64);
            }
        }
        Ok(())
    }

    /// One DRed round over the deltas `active` marks nonempty: a fill, integration or clear
    /// whose deltas are all empty is skipped. The round leaves the SCC's own deltas cleared
    /// and marks the variable deltas the next round reads.
    fn round(&self, db: &Connection, scc_id: usize, phase: Phase, active: &mut [bool], counters: &mut Counters) -> rusqlite::Result<bool> {
        let deleting = phase == Phase::Delete;
        let _round = tracing::debug_span!(target: crate::observe::TARGET, "frontier_round", scc = scc_id, deleting).entered();
        let scc = &self.sccs[scc_id];
        let (phase_fills, phase_io) = if deleting { (&scc.delete_fills, &scc.delete_io) } else { (&scc.insert_fills, &scc.insert_io) };
        let live = |active: &[bool], reads: &[usize]| reads.is_empty() || reads.iter().any(|id| active[*id]);
        let mut minted = false;
        for (sql, (reads, write)) in phase_fills.iter().zip(phase_io) {
            if sql.starts_with(crate::terms::AFTER_MINT) {
                if minted { Self::exec(db, sql, counters)?; }
                continue;
            }
            minted = false;
            if !live(active, reads) { continue; }
            let rows = Self::exec(db, sql, counters)?;
            minted = rows > 0;
            if let Some(id) = write { active[*id] |= rows > 0; }
        }
        for (sql, reads) in scc.integrates.iter().zip(&scc.integrate_reads) {
            if live(active, reads) { Self::exec(db, sql, counters)?; }
        }
        self.count_work(db, Owner::Loop(scc_id), Some(&*active), counters)?;
        Self::exec_all(
            db,
            scc.vars.iter().map(|v| match phase {
                Phase::Delete => &v.over_delete,
                Phase::Insert => &v.insert,
                Phase::Recompute => &v.step,
            }),
            counters,
        )?;
        for (sql, target) in scc.clears.iter().zip(&scc.clear_targets) {
            if target.is_none_or(|id| active[id]) { Self::exec(db, sql, counters)?; }
        }
        for id in scc.clear_targets.iter().flatten() { active[*id] = false; }
        let mut more = false;
        for (v, id) in scc.vars.iter().zip(&scc.var_ids) {
            Self::exec_all(db, [&v.to_delta, &v.to_acc], counters)?;
            if deleting {
                Self::exec(db, &v.to_del, counters)?;
            }
            Self::exec(db, &v.clear_nx, counters)?;
            let any = sqlite_ext::statements::query_cached(db, "settle", object_of(&v.any), &v.any, [], |r| r.get::<_, bool>(0))?;
            active[*id] = any;
            more |= any;
            *counters.statements.as_mut().unwrap() += 1;
        }
        if more { *counters.rounds.as_mut().unwrap() += 1; }
        Ok(more)
    }

    /// The recompute path: Materialize `WITH MUTUALLY RECURSIVE` evaluation from empty variables.
    /// The old value goes into `acc` at -1; every loop-owned image and private table empties; each
    /// copy's delta becomes its input's whole image. Each round fills the loop from the changed
    /// deltas, integrates, and sets each variable to the support of its body's image (`step`); the
    /// result delta is `acc`, new value minus old. Evaluation `limit + 1` that still changes a
    /// variable stops the settle with `LetRecLimitHit`.
    fn recompute(&self, db: &Connection, scc_id: usize, outer: &[bool], counters: &mut Counters) -> rusqlite::Result<Vec<bool>> {
        let span = tracing::debug_span!(target: crate::observe::TARGET, "frontier_recompute", scc = scc_id,
            rounds = tracing::field::Empty);
        let _recompute = span.enter();
        let scc = &self.sccs[scc_id];
        let mut active = outer.to_vec();
        Self::exec_all(db, scc.vars.iter().map(|v| &v.capture), counters)?;
        Self::exec_all(db, &scc.reset, counters)?;
        for id in &scc.copy_ids { active[*id] = true; }
        for id in &scc.var_ids { active[*id] = false; }
        let mut evaluation = 0u32;
        while self.round(db, scc_id, Phase::Recompute, &mut active, counters)? {
            evaluation += 1;
            if scc.limit.is_some_and(|limit| evaluation > limit) {
                span.record("rounds", evaluation);
                return Err(rusqlite::Error::UserFunctionError(Box::new(LetRecLimitHit { rel: scc.first, limit: scc.limit.unwrap_or(0) })));
            }
        }
        span.record("rounds", evaluation + 1);
        tracing::debug!(target: crate::observe::TARGET, scc = scc_id, evaluations = evaluation + 1, "LetRec recomputed");
        let mut results = Vec::with_capacity(scc.vars.len());
        for v in &scc.vars {
            results.push(Self::exec(db, &v.finish, counters)? > 0);
            Self::exec(db, &v.clear_acc, counters)?;
        }
        Ok(results)
    }

    /// `outer` marks the settle's nonempty deltas; inside the SCC only copies of them are read.
    fn fixpoint(&self, db: &Connection, scc_id: usize, outer: &[bool], counters: &mut Counters) -> rusqlite::Result<Vec<bool>> {
        let span = tracing::debug_span!(target: crate::observe::TARGET, "frontier_fixpoint", scc = scc_id,
            delete_rounds = tracing::field::Empty, insert_rounds = tracing::field::Empty);
        let _fixpoint = span.enter();
        let scc = &self.sccs[scc_id];
        let mut active = outer.to_vec();
        Self::exec_all(db, &scc.stash, counters)?;
        for (sql, write) in scc.delete_seeds.iter().zip(&scc.delete_seed_writes) {
            let rows = Self::exec(db, sql, counters)?;
            if let Some(id) = write { active[*id] |= rows > 0; }
        }
        let mut rounds = 1u64;
        while self.round(db, scc_id, Phase::Delete, &mut active, counters)? { rounds += 1; }
        span.record("delete_rounds", rounds);
        for (v, id) in scc.vars.iter().zip(&scc.var_ids) {
            Self::exec_all(db, [&v.rederive, &v.clear_del], counters)?;
            active[*id] |= Self::exec(db, &v.to_delta, counters)? > 0;
            Self::exec_all(db, [&v.to_acc, &v.clear_nx], counters)?;
        }
        Self::exec_all(db, &scc.restore, counters)?;
        for id in scc.clear_targets.iter().flatten() { active[*id] |= outer[*id]; }
        for (sql, write) in scc.insert_seeds.iter().zip(&scc.insert_seed_writes) {
            let rows = Self::exec(db, sql, counters)?;
            if let Some(id) = write { active[*id] |= rows > 0; }
        }
        let mut rounds = 1u64;
        while self.round(db, scc_id, Phase::Insert, &mut active, counters)? { rounds += 1; }
        span.record("insert_rounds", rounds);
        let mut results = Vec::with_capacity(scc.vars.len());
        for v in &scc.vars {
            results.push(Self::exec(db, &v.finish, counters)? > 0);
            Self::exec(db, &v.clear_acc, counters)?;
        }
        Ok(results)
    }

    /// Registers `work_function` on the connection that settles this plan.
    pub(crate) fn register_work(&self, db: &Connection) -> rusqlite::Result<()> {
        let rows = self.work_rows.clone();
        db.create_scalar_function(self.work_function.as_str(), 2, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |ctx| {
            let (slot, count) = (ctx.get::<i64>(0)? as usize, ctx.get::<i64>(1)? as u64);
            let mut rows = rows.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
            *rows.get_mut(slot).ok_or(rusqlite::Error::InvalidQuery)? += count;
            Ok(true)
        })
    }

    pub fn run(&self, db: &Connection, counters: &mut Counters) -> rusqlite::Result<Vec<(Row, W)>> {
        if !self.filter_measured { counters.delta_rows.filter = None; }
        if !self.mint_measured { counters.delta_rows.mint = None; }
        self.work_rows.lock().map_err(|_| rusqlite::Error::InvalidQuery)?.iter_mut().for_each(|rows| *rows = 0);
        let mut active = vec![false; self.node_count];
        let staged = if self.source_tables.is_empty() { Vec::new() } else {
            *counters.statements.as_mut().unwrap() += 1;
            db.prepare_cached(&self.staged_sql)?.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        // A staged source is read as changed; rows that net to zero leave its CTE empty.
        for (id, table) in self.source_ids.iter().zip(&self.source_tables) {
            if let Some(id) = id { active[*id] |= staged.contains(table); }
        }
        let mut minted = false;
        for (at, step) in self.steps.iter().enumerate() {
            match step {
                Step::Fill(sql) if sql.starts_with(crate::terms::AFTER_MINT) => {
                    if minted { Self::exec(db, sql, counters)?; }
                }
                Step::Fill(sql) => {
                    minted = false;
                    let reads = &self.step_reads[at];
                    if !reads.is_empty() && !reads.iter().any(|id| active.get(*id) == Some(&true)) { continue; }
                    let rows = Self::exec(db, sql, counters)?;
                    minted = rows > 0;
                    if let Some(id) = self.step_writes[at] { active[id] |= rows > 0; }
                }
                Step::Loop(s) => {
                    let reads = &self.scc_reads[*s];
                    if !reads.is_empty() && !reads.iter().any(|id| active.get(*id) == Some(&true)) { continue; }
                    let results = if self.sccs[*s].nonmonotone {
                        self.recompute(db, *s, &active, counters)?
                    } else {
                        self.fixpoint(db, *s, &active, counters)?
                    };
                    for (id, nonempty) in self.scc_results[*s].iter().zip(results) {
                        active[*id] |= nonempty;
                    }
                }
            }
        }
        self.count_work(db, Owner::Settle, Some(&active), counters)?;
        let statement = sqlite_ext::statements::open("settle", "output", &self.output_delta, sqlite_ext::statements::CACHED);
        let _statement = statement.enter();
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
        statement.rows(changes.len());
        drop(_statement);
        *counters.statements.as_mut().unwrap() += 1;
        for (sql, reads) in self.integrates.iter().zip(&self.integrate_reads) {
            if reads.is_empty() || reads.iter().any(|id| active.get(*id) == Some(&true)) {
                Self::exec(db, sql, counters)?;
            }
        }
        for (sql, target) in self.clears.iter().zip(&self.clear_targets) {
            if target.is_none_or(|id| active.get(id) == Some(&true)) {
                Self::exec(db, sql, counters)?;
            }
        }
        Ok(changes)
    }
}

/// A recompute-path LetRec still changed a variable in body evaluation `limit + 1`; `rel` is the
/// LetRec's first id. Travels inside `rusqlite::Error::UserFunctionError` out of `NodesPlan::run`.
#[derive(Debug)]
pub(crate) struct LetRecLimitHit {
    pub rel: Option<RelId>,
    pub limit: u32,
}

impl std::fmt::Display for LetRecLimitHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LetRec limit {} reached (rel {:?})", self.limit, self.rel)
    }
}

impl std::error::Error for LetRecLimitHit {}

/// Table a settle statement writes (`INSERT INTO t`, `DELETE FROM t`) or reads (`FROM t`), for its span.
fn object_of(sql: &str) -> &str {
    let tail = sql.rsplit_once("INSERT INTO ")
        .or_else(|| sql.split_once("DELETE FROM "))
        .or_else(|| sql.split_once("FROM "))
        .map_or("", |(_, tail)| tail);
    tail.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap_or("")
}

fn ddl_object(sql: &str) -> Option<(String, String)> {
    let words = sql.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["CREATE", kind @ ("TABLE" | "INDEX"), name, ..] => Some(((*kind).into(), (*name).into())),
        _ => None,
    }
}
