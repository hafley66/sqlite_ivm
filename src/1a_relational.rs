//! Every operator emits the row stored in its arrangement.
//! Equality keys select membership, while stored row values select emitted identity.
use crate::{
    query::{error, quote},
    relational::{Occurrence, Plan, Rule},
};
use rusqlite::{types::Value, Connection, Result};
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
pub(crate) fn delta_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_delta_{width}_{id}_{side}")
}
pub(crate) fn delta_index(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_delta_{width}_{id}_{side}_r")
}
/// Fixpoint scratch: one side's rows that entered its arrangement this batch.
pub(crate) fn arrived_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_arrived_{width}_{id}_{side}")
}
/// Fixpoint scratch: one side's rows that left its arrangement this batch.
pub(crate) fn left_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_left_{width}_{id}_{side}")
}
/// Fixpoint scratch: members removed by the delete pass, keyed as stored.
pub(crate) fn deleted_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_deleted_{width}_{id}")
}
pub(crate) fn parameters(n: usize) -> String {
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
