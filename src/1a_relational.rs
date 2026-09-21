//! Every operator emits the row stored in its arrangement.
//! Equality keys select membership, while stored row values select emitted identity.
use crate::{
    query::{error, quote},
    relational::{Kind, Occurrence, Plan, Rule},
};
use rusqlite::{params_from_iter, types::Value, Connection, Result};
pub type Row = Vec<Value>;
pub(crate) fn columns(n: usize) -> String {
    (0..n)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",")
}
pub(crate) fn table(name: &str, id: usize, side: usize) -> String {
    format!("main.{}", quote(&format!("{name}_op{id}_{side}")))
}
/// Upsert scratch: one side's delta summed per identity, the identity and its
/// hash computed once per row instead of once per comparison.
fn delta_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_delta_{width}_{id}_{side}")
}
fn delta_index(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_delta_{width}_{id}_{side}_r")
}
/// Fixpoint scratch: one side's rows that entered its arrangement this batch.
fn arrived_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_arrived_{width}_{id}_{side}")
}
/// Fixpoint scratch: one side's rows that left its arrangement this batch.
fn left_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_left_{width}_{id}_{side}")
}
/// Fixpoint scratch: members removed by the delete pass, keyed as stored.
fn deleted_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_deleted_{width}_{id}")
}
fn parameters(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",")
}
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// FNV-1a 64 over the identity bytes. A stored hash must survive a toolchain
/// upgrade, and std's DefaultHasher promises no algorithm stability.
pub fn row_hash(bytes: &[u8]) -> i64 {
    bytes
        .iter()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(FNV_PRIME)
        }) as i64
}
/// Maintenance cannot run without a Plan and a Plan only comes from `bind`,
/// so registering there reaches every connection that can issue this SQL.
pub fn register_functions(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"sqlite_ivm_hash",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(row_hash(ctx.get_raw(0).as_str()?.as_bytes())),
    )
}
pub fn keys_table(name: &str) -> String {
    format!("main.{}", quote(&format!("{name}_keys")))
}
/// Interning is idempotent and monotone: one composite takes one id for the
/// life of the view, so two equal composites can never reach two ids.
pub fn intern(db: &Connection, dict: &str, value: &str) -> Result<i64> {
    // One seek on hit and on miss: the conflict arm updates nothing and still
    // returns the existing id.
    db.prepare_cached(&format!(
        "INSERT INTO {dict}(__v) VALUES(?1) ON CONFLICT(__v) DO UPDATE SET __v=__v RETURNING __i"
    ))?
    .query_row([value], |r| r.get(0))
}
pub fn resolve(db: &Connection, dict: &str, id: i64) -> Result<String> {
    db.prepare_cached(&format!("SELECT __v FROM {dict} WHERE __i=?1"))?
        .query_row([id], |r| r.get(0))
}
pub(crate) fn max_rowid(db: &Connection, t: &str) -> Result<i64> {
    db.prepare_cached(&format!("SELECT coalesce(max(rowid),0) FROM {t}"))?
        .query_row([], |r| r.get(0))
}
pub(crate) enum Role {
    Table(String),
    Range(String, i64, i64),
}
/// Every occurrence becomes a flattenable subquery renaming its columns into
/// the rule's shared `c{i}` namespace; `?` numbering continues from `params`.
pub(crate) fn rule_from(rule: &Rule, roles: &[Role], params: &mut Vec<Value>) -> String {
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
/// `bound` names the changed side's delta table and which occurrence of that
/// side reads it; every other occurrence reads the full arrangement, so a rule
/// that mentions the side twice is driven once per occurrence.
pub(crate) fn roles(
    name: &str,
    id: usize,
    side: usize,
    rule: &Rule,
    bound: Option<(usize, &str)>,
    member: Role,
) -> Vec<Role> {
    rule.occurrences
        .iter()
        .enumerate()
        .map(|(n, (o, _))| match (o, bound) {
            (Occurrence::Input(s), Some((at, delta))) if *s == side && n == at => {
                Role::Table(delta.to_string())
            }
            (Occurrence::Input(s), _) => Role::Table(table(name, id, *s)),
            (Occurrence::Member, _) => match &member {
                Role::Table(t) => Role::Table(t.clone()),
                Role::Range(t, lo, hi) => Role::Range(t.clone(), *lo, *hi),
            },
        })
        .collect()
}
pub(crate) fn rule_where(rule: &Rule, extra: Option<&str>) -> String {
    match (&rule.predicate, extra) {
        (None, None) => String::new(),
        (Some(p), None) => format!(" WHERE {p}"),
        (None, Some(e)) => format!(" WHERE {e}"),
        (Some(p), Some(e)) => format!(" WHERE {p} AND {e}"),
    }
}
pub(crate) trait CachedExecute {
    fn execute_cached<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize>;
}
impl CachedExecute for Connection {
    fn execute_cached<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize> {
        self.prepare_cached(sql)?.execute(params)
    }
}
pub(crate) const BULK_ROUND_BUDGET: usize = 100_000;
pub(crate) const BULK_GROUP_BUDGET: usize = 100_000;
pub(crate) const BULK_MULTIPLICITY_BUDGET: i64 = 1_000_000;
pub(crate) fn out_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_out_{width}_{id}")
}
pub(crate) fn json_key(parts: Vec<String>) -> String {
    format!("json_array({})", parts.join(","))
}
pub(crate) fn folded(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN CASE WHEN {value}=CAST({value} AS INTEGER) AND typeof(CAST({value} AS INTEGER))='integer' THEN CAST({value} AS INTEGER) ELSE json_object('real',sqlite_ivm_real_hex({value})) END WHEN 'text' THEN {value}||'' ELSE {value} END")
}
/// The composite an arrangement row rebuilds from its own stored columns.
/// `fill` writes it and `change` compares against it, so the two must agree.
pub fn identity_sql(width: usize) -> String {
    json_key((0..width).map(|i| plain(&format!("c{i}"))).collect())
}
pub(crate) fn plain(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN json_object('real',sqlite_ivm_real_hex({value})) WHEN 'text' THEN {value}||'' ELSE {value} END")
}
impl Plan {
    /// The temp scratch every drain writes: one out table and one before table
    /// per node, plus the touched-key set. Runs at bind time, outside any
    /// trigger program, because DDL inside a trigger aborts the statement.
    pub fn prepare_scratch(&self, db: &Connection) -> Result<()> {
        db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS __ivm_touched(__k INTEGER PRIMARY KEY)")?;
        for (id, node) in self.nodes.iter().enumerate() {
            let width = node.fields.len();
            db.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {}({cols},__m); CREATE TABLE IF NOT EXISTS temp.__ivm_before_{width}_{id}({cols},__m)",
                out_table(id, width),
                cols = columns(width)
            ))?;
            if matches!(node.kind, Kind::Set(_) | Kind::Join { .. } | Kind::Group { .. } | Kind::Fixpoint { .. }) {
                for side in 0..node.inputs.len() {
                    let side_width = self.nodes[node.inputs[side]].fields.len();
                    db.execute_batch(&format!(
                        "CREATE TABLE IF NOT EXISTS {}(__r INTEGER NOT NULL,__v TEXT NOT NULL,__n INTEGER NOT NULL,{}); CREATE INDEX IF NOT EXISTS {}_r ON {}(__r)",
                        delta_table(id, side, side_width),
                        columns(side_width),
                        delta_index(id, side, side_width),
                        delta_table(id, side, side_width).trim_start_matches("temp.")
                    ))?;
                }
            }
            if let Kind::Fixpoint { .. } = node.kind {
                for side in 0..node.inputs.len() {
                    let side_width = self.nodes[node.inputs[side]].fields.len();
                    let side_cols = columns(side_width);
                    db.execute_batch(&format!(
                        "CREATE TABLE IF NOT EXISTS {}({side_cols}); CREATE TABLE IF NOT EXISTS {}({side_cols})",
                        arrived_table(id, side, side_width),
                        left_table(id, side, side_width)
                    ))?;
                }
                db.execute_batch(&format!(
                    "CREATE TABLE IF NOT EXISTS {}(__k TEXT PRIMARY KEY,{})",
                    deleted_table(id, width),
                    columns(width)
                ))?;
            }
        }
        Ok(())
    }
    /// Set-at-a-time maintenance of one batch. Every node kind runs a constant
    /// number of statements per batch; row counts live inside SQLite. Node ids
    /// are topological because `push` appends after its inputs.
    pub fn drain(&self, db: &Connection, name: &str, batch: &[(usize, Row, i64)]) -> Result<()> {
        let _span = tracing::debug_span!("drain", view = name, rows = batch.len()).entered();
        let seed = tracing::debug_span!("node", kind = "seed", id = self.nodes.len()).entered();
        // One prepared insert per input node; the batch is the only per-row loop.
        for (id, node) in self.nodes.iter().enumerate() {
            let Kind::Input(input) = node.kind else {
                continue;
            };
            let width = self.sources[input].columns.len();
            let mut insert = db.prepare_cached(&format!(
                "INSERT INTO {} VALUES({})",
                out_table(id, width),
                parameters(width + 1)
            ))?;
            for (source, row, d) in batch {
                if *source != input || *d == 0 {
                    continue;
                }
                insert.execute(params_from_iter(row.iter().chain(std::iter::once(&Value::Integer(*d)))))?;
            }
        }
        drop(seed);
        let dict = keys_table(name);
        for id in 0..self.nodes.len() {
            let node = &self.nodes[id];
            let width = node.fields.len();
            let out = out_table(id, width);
            let cols = columns(width);
            let touched_inputs = node
                .inputs
                .iter()
                .map(|input| {
                    let child = out_table(*input, self.nodes[*input].fields.len());
                    db.prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM {child})"))?
                        .query_row([], |r| r.get::<_, bool>(0))
                })
                .collect::<Result<Vec<bool>>>()?;
            if !touched_inputs.iter().any(|t| *t) {
                continue;
            }
            let _node = tracing::debug_span!("node", kind = node.kind.label(), id).entered();
            match &node.kind {
                Kind::Input(_) => {}
                Kind::Map { .. } => self.materialize(db, name, id, false)?,
                Kind::Set(op) if *op == "all" => {
                    for input in &node.inputs {
                        let child = out_table(*input, self.nodes[*input].fields.len());
                        db.execute_cached(
                            &format!("INSERT INTO {out}({cols},__m) SELECT {cols},__m FROM {child}"),
                            [],
                        )?;
                    }
                }
                Kind::Set(_) | Kind::Join { .. } | Kind::Group { .. } => {
                    // Touched keys: every key of every input delta row, interned.
                    db.execute_cached("DELETE FROM temp.__ivm_touched", [])?;
                    for side in 0..node.inputs.len() {
                        let child = out_table(node.inputs[side], self.nodes[node.inputs[side]].fields.len());
                        let key = self.key_sql(id, side).ok_or_else(|| error("arrangement node without a key"))?;
                        db.execute_cached(
                            &format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {key} FROM {child}"),
                            [],
                        )?;
                        db.execute_cached(
                            &format!("INSERT OR IGNORE INTO temp.__ivm_touched SELECT __i FROM {dict} WHERE __v IN (SELECT {key} FROM {child})"),
                            [],
                        )?;
                    }
                    // Before: the node's output over the touched keys, parked.
                    let before = format!("temp.__ivm_before_{width}_{id}");
                    self.materialize(db, name, id, true)?;
                    db.execute_cached(&format!("INSERT INTO {before} SELECT * FROM {out}"), [])?;
                    db.execute_cached(&format!("DELETE FROM {out}"), [])?;
                    // Apply every input delta to its side's arrangement.
                    for (side, touched) in touched_inputs.iter().enumerate() {
                        if *touched {
                            self.upsert(db, name, id, side)?;
                        }
                    }
                    // After, then out = after minus before as a bag.
                    self.materialize(db, name, id, true)?;
                    let identity = json_key((0..width).map(|i| plain(&format!("c{i}"))).collect());
                    for sql in [
                        format!("INSERT INTO {out} SELECT {cols},-__m FROM {before}"),
                        format!("DELETE FROM {before}"),
                        format!("INSERT INTO {before} SELECT {cols},sum(__m) FROM {out} GROUP BY {identity} HAVING sum(__m)!=0"),
                        format!("DELETE FROM {out}"),
                        format!("INSERT INTO {out} SELECT * FROM {before}"),
                        format!("DELETE FROM {before}"),
                    ] {
                        db.execute_cached(&sql, [])?;
                    }
                }
                Kind::Fixpoint { rules } => {
                    for (side, touched) in touched_inputs.iter().enumerate() {
                        if *touched {
                            let child = out_table(node.inputs[side], self.nodes[node.inputs[side]].fields.len());
                            let key = self.key_sql(id, side).ok_or_else(|| error("arrangement node without a key"))?;
                            db.execute_cached(
                                &format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {key} FROM {child}"),
                                [],
                            )?;
                            self.split_side(db, name, id, side)?;
                            self.upsert(db, name, id, side)?;
                            self.fixpoint(db, name, id, side, rules)?;
                        }
                    }
                }
            }
            if id == self.output {
                let _apply = tracing::debug_span!("node", kind = "apply_state", id).entered();
                self.apply_state(db, name, id)?;
            }
        }
        // Sweep is the only clear: seed trusts it, and a failed drain aborts the
        // enclosing statement, which unwinds the temp writes with it.
        let _sweep = tracing::debug_span!("node", kind = "sweep", id = self.nodes.len()).entered();
        for (id, node) in self.nodes.iter().enumerate() {
            db.execute_cached(&format!("DELETE FROM {}", out_table(id, node.fields.len())), [])?;
        }
        Ok(())
    }
    /// Adds one side's delta rows (in that input's out table) to the
    /// arrangement. Existing identities take the summed multiplicity; new
    /// identities are inserted; zero rows leave; a negative row is an error.
    fn upsert(&self, db: &Connection, name: &str, id: usize, side: usize) -> Result<()> {
        let node = &self.nodes[id];
        let child_width = self.nodes[node.inputs[side]].fields.len();
        let child = out_table(node.inputs[side], child_width);
        let t = table(name, id, side);
        let cols = columns(child_width);
        let identity = identity_sql(child_width);
        let identity_of = |alias: &str| json_key((0..child_width).map(|i| plain(&format!("{alias}.c{i}"))).collect());
        let key = self.key_sql(id, side).ok_or_else(|| error("arrangement node without a key"))?;
        let dict = keys_table(name);
        let delta = delta_table(id, side, child_width);
        let arrangement_identity = identity_of(&t);
        db.execute_cached(&format!("DELETE FROM {delta}"), [])?;
        db.execute_cached(
            &format!(
                "INSERT INTO {delta}(__r,__v,__n,{cols}) SELECT sqlite_ivm_hash(__ivm_v),__ivm_v,__ivm_n,{cols} \
                 FROM (SELECT {identity} AS __ivm_v,sum(__m) AS __ivm_n,{cols} FROM {child} GROUP BY {identity}) WHERE __ivm_n!=0"
            ),
            [],
        )?;
        db.execute_cached(
            &format!(
                "UPDATE {t} SET __n={t}.__n+d.__n FROM {delta} d WHERE d.__r={t}.__r AND d.__v={arrangement_identity}"
            ),
            [],
        )?;
        db.execute_cached(
            &format!(
                "INSERT INTO {t}(__k,__r,__n,{cols}) SELECT (SELECT __i FROM {dict} WHERE __v={key}),d.__r,d.__n,{cols} FROM {delta} d \
                 WHERE NOT EXISTS(SELECT 1 FROM {t} a WHERE a.__r=d.__r AND {}=d.__v)",
                identity_of("a")
            ),
            [],
        )?;
        let bad: bool = db
            .prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE __n<0 OR typeof(__n)!='integer')"))?
            .query_row([], |r| r.get(0))?;
        if bad {
            return Err(error("negative arrangement multiplicity"));
        }
        db.execute_cached(&format!("DELETE FROM {t} WHERE __n=0"), [])?;
        Ok(())
    }
    /// Applies the output node's delta to the result rows: retractions delete
    /// that many copies by key, additions insert that many copies.
    fn apply_state(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let width = self.nodes[id].fields.len();
        let out = out_table(id, width);
        let state = format!("main.{}", quote(&format!("{name}_state")));
        let key = json_key((0..width).map(|i| plain(&format!("o.c{i}"))).collect());
        let wanted: i64 = db
            .prepare_cached(&format!("SELECT coalesce(sum(-__m),0) FROM {out} o WHERE __m<0"))?
            .query_row([], |r| r.get(0))?;
        let removed = db.execute_cached(
            &format!(
                "DELETE FROM {state} WHERE rowid IN (SELECT s.rowid FROM {state} s JOIN (SELECT {key} AS __key,sum(-__m) AS __n FROM {out} o WHERE __m<0 GROUP BY {key}) d ON s.__key=d.__key \
                 WHERE (SELECT count(*) FROM {state} p WHERE p.__key=s.__key AND p.rowid<=s.rowid)<=d.__n)"
            ),
            [],
        )?;
        if removed as i64 != wanted {
            return Err(error(format!(
                "missing result multiplicity: {wanted} retractions, {removed} rows present"
            )));
        }
        let peak: i64 = db
            .prepare_cached(&format!("SELECT coalesce(max(__m),0) FROM {out}"))?
            .query_row([], |r| r.get(0))?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        db.execute_cached(
            &format!(
                "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce(max(__m),0) FROM {out})) \
                 INSERT INTO {state}(__key,{}) SELECT {key},o.c0{} FROM {out} o,seq WHERE o.__m>0 AND seq.n<=o.__m",
                columns(width),
                (1..width).map(|i| format!(",o.c{i}")).collect::<String>()
            ),
            [],
        )?;
        Ok(())
    }
    /// Delete-and-rederive over one member set. Insertion and rederivation run
    /// semi-naive rounds whose delta is a rowid range of the member table.
    #[allow(clippy::too_many_arguments)]
    /// Splits one side's delta into the rows entering its arrangement and the
    /// rows leaving it, by net multiplicity against what is stored.
    fn split_side(&self, db: &Connection, name: &str, id: usize, side: usize) -> Result<()> {
        let node = &self.nodes[id];
        let width = self.nodes[node.inputs[side]].fields.len();
        let child = out_table(node.inputs[side], width);
        let t = table(name, id, side);
        let arrived = arrived_table(id, side, width);
        let left = left_table(id, side, width);
        let cols = columns(width);
        let identity_of = |alias: &str| json_key((0..width).map(|i| plain(&format!("{alias}.c{i}"))).collect());
        let delta_identity = identity_of("o");
        let stored_identity = identity_of("a");
        let net = format!(
            "(SELECT sum(o.__m) FROM {child} o WHERE sqlite_ivm_hash({delta_identity})=a.__r AND {delta_identity}={stored_identity})"
        );
        db.execute_cached(&format!("DELETE FROM {arrived}"), [])?;
        db.execute_cached(&format!("DELETE FROM {left}"), [])?;
        db.execute_cached(
            &format!(
                "INSERT INTO {left} SELECT {cols} FROM {t} a \
                 WHERE a.__r IN (SELECT sqlite_ivm_hash({delta_identity}) FROM {child} o) AND a.__n+coalesce({net},0)=0"
            ),
            [],
        )?;
        db.execute_cached(
            &format!(
                "INSERT INTO {arrived} SELECT {cols} FROM (SELECT {identity} AS __ivm_r,sum(__m) AS __ivm_n,{cols} FROM {child} GROUP BY {identity}) o \
                 WHERE __ivm_n>0 AND NOT EXISTS(SELECT 1 FROM {t} a WHERE a.__r=sqlite_ivm_hash(o.__ivm_r) AND {stored_identity}=o.__ivm_r)",
                identity = identity_sql(width)
            ),
            [],
        )?;
        Ok(())
    }
    /// Semi-naive closure over one side's arrived and left sets. Arrivals
    /// derive forward from the new rows; departures delete every member they
    /// reached, then rederive what survives another way. Deltas land in the
    /// node's out table.
    fn fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        side: usize,
        rules: &[Rule],
    ) -> Result<()> {
        let span = tracing::debug_span!(
            "fixpoint",
            view = name,
            node = id,
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
        let width = node.fields.len();
        let cols = columns(width);
        let out = out_table(id, width);
        let all = table(name, id, node.inputs.len());
        let work = table(name, id, node.inputs.len() + 1);
        let side_width = self.nodes[node.inputs[side]].fields.len();
        let arrived = arrived_table(id, side, side_width);
        let left = left_table(id, side, side_width);
        let deleted = deleted_table(id, width);
        let derive = |target: &str,
                      rule: &Rule,
                      roles: &[Role],
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
        // One derive per occurrence of the changed side, reading `delta` there.
        let derive_side = |target: &str, delta: &str, only_present: bool| -> Result<()> {
            for rule in rules.iter().filter(|r| r.mentions(side)) {
                for (at, _) in rule
                    .occurrences
                    .iter()
                    .enumerate()
                    .filter(|(_, (o, _))| *o == Occurrence::Input(side))
                {
                    derive(
                        target,
                        rule,
                        &roles(name, id, side, rule, Some((at, delta)), Role::Table(all.clone())),
                        only_present,
                    )?;
                }
            }
            Ok(())
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

        // Every member past `lo` at the end is new unless the delete pass stored
        // it first; rederived rows take fresh rowids, so `lo` precedes that pass.
        let lo = max_rowid(db, &all)?;
        // Departures first: delete everything the left rows reached, rederive.
        let any_left: bool = db
            .prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM {left})"))?
            .query_row([], |r| r.get(0))?;
        if any_left {
            db.execute_cached(&format!("DELETE FROM {work}"), [])?;
            db.execute_cached(&format!("DELETE FROM {deleted}"), [])?;
            derive_side(&work, &left, true)?;
            let mut lo = 0;
            let mut delete_rounds = 0usize;
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
                db.execute_cached(
                    &format!("INSERT OR IGNORE INTO {deleted}(__k,{cols}) SELECT __k,{cols} FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"),
                    rusqlite::params![lo, hi],
                )?;
                db.execute_cached(
                    &format!("DELETE FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"),
                    rusqlite::params![lo, hi],
                )?;
                let mut written = 0;
                for rule in rules.iter().filter(|r| r.member().is_some()) {
                    written += derive(
                        &work,
                        rule,
                        &roles(name, id, side, rule, None, Role::Range(work.clone(), lo, hi)),
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
            db.execute_cached(&format!("DELETE FROM {work}"), [])?;
        }

        // Arrivals: derive forward from the new rows, then close.
        derive_side(&all, &arrived, false)?;
        rounds(lo)?;

        // Deltas: a deleted member gone for good retracts; one stored again with
        // another representative retracts the old row and emits the new one;
        // rows past `lo` that were not deleted this pass are new.
        if any_left {
            let d_identity = json_key((0..width).map(|i| plain(&format!("d.c{i}"))).collect());
            let a_identity = json_key((0..width).map(|i| plain(&format!("a.c{i}"))).collect());
            let d_cols = (0..width).map(|i| format!("d.c{i}")).collect::<Vec<_>>().join(",");
            let a_cols = (0..width).map(|i| format!("a.c{i}")).collect::<Vec<_>>().join(",");
            db.execute_cached(
                &format!(
                    "INSERT INTO {out}({cols},__m) SELECT {d_cols},-1 FROM {deleted} d \
                     WHERE NOT EXISTS(SELECT 1 FROM {all} a WHERE a.__k=d.__k AND {a_identity}={d_identity})"
                ),
                [],
            )?;
            db.execute_cached(
                &format!(
                    "INSERT INTO {out}({cols},__m) SELECT {a_cols},1 FROM {deleted} d JOIN {all} a ON a.__k=d.__k \
                     WHERE {a_identity}!={d_identity}"
                ),
                [],
            )?;
            db.execute_cached(
                &format!(
                    "INSERT INTO {out}({cols},__m) SELECT {a_cols},1 FROM {all} a WHERE a.rowid>?1 \
                     AND NOT EXISTS(SELECT 1 FROM {deleted} d WHERE d.__k=a.__k)"
                ),
                [lo],
            )?;
            db.execute_cached(&format!("DELETE FROM {deleted}"), [])?;
        } else {
            db.execute_cached(
                &format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 FROM {all} WHERE rowid>?1"),
                [lo],
            )?;
        }
        Ok(())
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
    let collector = plan.collector(name);
    collector.create_shadow(db)?;
    objects.push(("table", collector.shadow_table()));
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
