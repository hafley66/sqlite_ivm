//! Signed rows retain their values through source reads and result updates.
//! Equality keys select membership; exact native cells select result identity.
use crate::{
    catalog::{error, quote},
    statements::{self, Phase},
    relational::{Kind, Occurrence, Plan, Rule},
};
use rusqlite::{types::Value, Connection, Result};
pub type Row = Vec<Value>;
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn columns(n: usize) -> String {
    (0..n)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",")
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn table(name: &str, id: usize, side: usize) -> String {
    format!("main.{}", quote(&format!("{name}_op{id}x{side}")))
}
/// Fixpoint scratch: one side's rows that entered its arrangement this batch.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn arrived_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_arrived_{width}_{id}_{side}")
}
/// Fixpoint scratch: one side's rows that left its arrangement this batch.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn left_table(id: usize, side: usize, width: usize) -> String {
    format!("temp.__ivm_left_{width}_{id}_{side}")
}
/// Fixpoint scratch: members removed by the delete pass, keyed as stored.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn deleted_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_deleted_{width}_{id}")
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
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
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn row_hash(bytes: &[u8]) -> i64 {
    bytes
        .iter()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(FNV_PRIME)
        }) as i64
}
/// Maintenance cannot run without a Plan and a Plan only comes from `bind`,
/// so registering there reaches every connection that can issue this SQL.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn register_functions(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"sqlite_ivm_real_hex",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let _callback = tracing::trace_span!("sqlite_ivm_real_hex").entered();
            let value: rusqlite::types::Value = ctx.get(0)?;
            match value {
                rusqlite::types::Value::Real(v) => Ok(Some(format!("{:016x}", v.to_bits()))),
                _ => Ok(None),
            }
        },
    )?;

    // A corruption check only. Row identity and index matching never use this
    // checksum; collisions therefore cannot merge rows or select a retraction.
    db.create_scalar_function(
        c"sqlite_ivm_row_check", -1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let _callback = tracing::trace_span!("sqlite_ivm_row_check").entered();
            use rusqlite::types::ValueRef;
            let mut hash = FNV_OFFSET;
            let mut feed = |bytes: &[u8]| {
                for byte in bytes { hash = (hash ^ *byte as u64).wrapping_mul(FNV_PRIME); }
            };
            for i in 0..ctx.len() {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                match ctx.get_raw(i) {
                    ValueRef::Null => feed(&[0]),
                    ValueRef::Integer(value) => { feed(&[1]); feed(&value.to_le_bytes()); }
                    ValueRef::Real(value) => { feed(&[2]); feed(&value.to_bits().to_le_bytes()); }
                    ValueRef::Text(value) => { feed(&[3]); feed(&(value.len() as u64).to_le_bytes()); feed(value); }
                    ValueRef::Blob(value) => { feed(&[4]); feed(&(value.len() as u64).to_le_bytes()); feed(value); }
                }
            }
            Ok(hash as i64)
        },
    )?;
    db.create_scalar_function(
        c"sqlite_ivm_hash",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(row_hash(ctx.get_raw(0).as_str()?.as_bytes())),
    )
}

#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains('\0')
        || name.to_ascii_lowercase().starts_with("sqlite_")
        || name.to_ascii_lowercase().starts_with("__ivm_")
    {
        return Err(error(
            "view name must be 1..128 bytes, NUL-free and non-reserved",
        ));
    }
    Ok(())
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn keys_table(name: &str) -> String {
    format!("main.{}", quote(&format!("{name}_keys")))
}
/// Interning is idempotent and monotone: one composite takes one id for the
/// life of the view, so two equal composites can never reach two ids.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn intern(db: &Connection, dict: &str, value: &str) -> Result<i64> {
    // One seek on hit and on miss: the conflict arm updates nothing and still
    // returns the existing id.
    statements::query_cached(
        db,
        Phase::Maintain,
        dict,
        &format!(
            "INSERT INTO {dict}(__v) VALUES(?1) ON CONFLICT(__v) DO UPDATE SET __v=__v RETURNING __i"
        ),
        [value],
        |r| r.get(0),
    )
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn resolve(db: &Connection, dict: &str, id: i64) -> Result<String> {
    statements::query_cached(
        db,
        Phase::Maintain,
        dict,
        &format!("SELECT __v FROM {dict} WHERE __i=?1"),
        [id],
        |r| r.get(0),
    )
}
pub(crate) enum Role {
    Table(String),
    Range(String, i64, i64),
}
/// Every occurrence becomes a flattenable subquery renaming its columns into
/// the rule's shared `c{i}` namespace; `?` numbering continues from `params`.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn rule_from(rule: &Rule, roles: &[Role], params: &mut Vec<Value>) -> String {
    let mut offset = 0;
    let mut sources = vec![];
    for (n, ((_, width), role)) in rule.occurrences.iter().zip(roles).enumerate() {
        tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
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
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn roles(
    plan: &Plan,
    db: &Connection,
    before: bool,
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
            (Occurrence::Input(s), _) => Role::Table(plan.fixpoint_rows(db, name, id, *s, before)),
            (Occurrence::Member, _) => match &member {
                Role::Table(t) => Role::Table(t.clone()),
                Role::Range(t, lo, hi) => Role::Range(t.clone(), *lo, *hi),
            },
        })
        .collect()
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn rule_where(rule: &Rule, extra: Option<&str>) -> String {
    match (&rule.predicate, extra) {
        (None, None) => String::new(),
        (Some(p), None) => format!(" WHERE {p}"),
        (None, Some(e)) => format!(" WHERE {e}"),
        (Some(p), Some(e)) => format!(" WHERE {p} AND {e}"),
    }
}
pub(crate) const BULK_ROUND_BUDGET: usize = 100_000;
pub(crate) const BULK_GROUP_BUDGET: usize = 100_000;
pub(crate) const BULK_MULTIPLICITY_BUDGET: i64 = 1_000_000;
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn out_table(id: usize, width: usize) -> String {
    format!("temp.__ivm_out_{width}_{id}")
}
impl Plan {
    /// Give non-overlapping delta lifetimes of the same width one temp table.
    /// Inputs start at the seed boundary; other nodes start at their topological
    /// position. A table is reusable only after its last consumer has run.
    pub(crate) fn assign_out_slots(&mut self) {
        let count = self.nodes.len();
        let mut last_reads: Vec<usize> = (0..count).collect();
        for (consumer, node) in self.nodes.iter().enumerate() {
            for &input in &node.inputs {
                last_reads[input] = last_reads[input].max(consumer);
            }
        }
        last_reads[self.output] = count;
        if crate::relational_program::aggregate_sum_inputs(self).is_some() {
            let input = self.nodes[self.output].inputs[0];
            last_reads[input] = count;
        }
        for (id, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind, Kind::Input(_)) && last_reads[id] == id {
                last_reads[id] = count;
            }
        }
        let mut slots: std::collections::HashMap<usize, Vec<(usize, usize)>> =
            std::collections::HashMap::new();
        self.out_slots = Vec::with_capacity(count);
        for (id, node) in self.nodes.iter().enumerate() {
            let start = if matches!(node.kind, Kind::Input(_)) { 0 } else { id };
            let width_slots = slots.entry(node.fields.len()).or_default();
            if let Some((slot, end)) = width_slots.iter_mut().find(|(_, end)| *end < start) {
                self.out_slots.push(*slot);
                *end = last_reads[id];
            } else {
                width_slots.push((id, last_reads[id]));
                self.out_slots.push(id);
            }
        }
        self.out_last_reads = last_reads;
        for (id, node) in self.nodes.iter().enumerate() {
            tracing::trace!(id, kind = node.kind.label(), inputs = ?node.inputs, width = node.fields.len(), out_slot = self.out_slots[id], last_read = self.out_last_reads[id], "scratch_output_slot");
        }
        tracing::info!(nodes = count, out_tables = slots.values().map(Vec::len).sum::<usize>(), "scratch_output_lifetimes");
    }

    pub(crate) fn out_table(&self, id: usize, width: usize) -> String {
        debug_assert_eq!(self.nodes[id].fields.len(), width);
        out_table(self.out_slots[id], width)
    }
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn json_key(parts: Vec<String>) -> String {
    format!("json_array({})", parts.join(","))
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn folded(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN CASE WHEN {value}=CAST({value} AS INTEGER) AND typeof(CAST({value} AS INTEGER))='integer' THEN CAST({value} AS INTEGER) ELSE json_object('real',sqlite_ivm_real_hex({value})) END WHEN 'text' THEN {value}||'' ELSE {value} END")
}
/// Encoded identity retained for set representatives and recursive membership.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn identity_sql(width: usize) -> String {
    json_key((0..width).map(|i| plain(&format!("c{i}"))).collect())
}
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn plain(value: &str) -> String {
    format!("CASE typeof({value}) WHEN 'blob' THEN json_object('blob',hex({value})) WHEN 'real' THEN json_object('real',sqlite_ivm_real_hex({value})) WHEN 'text' THEN {value}||'' ELSE {value} END")
}

#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn install(db: &Connection, name: &str, sql: &str, plan: &Plan) -> Result<()> {
    validate_name(name)?;
    let settings:bool=statements::query(db,Phase::Declare,name,"SELECT (SELECT recursive_triggers FROM pragma_recursive_triggers)=1 AND (SELECT trusted_schema FROM pragma_trusted_schema)=1",[],|r|r.get(0))?;
    if !settings {
        return Err(error(
            "sqlite_ivm requires recursive_triggers=ON and trusted_schema=ON",
        ));
    }
    let mut objects = plan.create_state(db, name)?;
    let collector = plan.collector(name);
    statements::guard(
        Phase::Declare,
        name,
        &format!("CREATE TABLE {}", collector.shadow_table()),
        || collector.create_shadow(db),
    )?;
    objects.push(("table", collector.shadow_table()));
    objects.extend(hooks(db, name, plan)?);
    let columns = plan
        .sources
        .iter()
        .enumerate()
        .flat_map(|(source, s)| {
            s.columns.iter().map(move |name| crate::catalog::Column {
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
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn hooks(db: &Connection, name: &str, plan: &Plan) -> Result<Vec<(&'static str, String)>> {
    let mut objects = vec![];
    for (source, s) in plan.sources.iter().enumerate() {
        tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
        for event in ["INSERT", "DELETE", "UPDATE"] {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            let trigger = format!("__ivm_{name}_{source}_{}_after", event.to_ascii_lowercase());
            let images = match event {
                "INSERT" => vec![("NEW", 1)],
                "DELETE" => vec![("OLD", 0)],
                _ => vec![("OLD", 0), ("NEW", 1)],
            };
            let mut body=String::from("SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)!=1 THEN RAISE(ABORT,'sqlite_ivm requires recursive_triggers=ON') END;");
            for (image, adding) in images {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
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
            // Stage OLD/NEW only after the source write succeeds. AFTER avoids
            // retracting writes rejected by OR IGNORE.
            statements::batch(
                db,
                Phase::Declare,
                name,
                &format!(
                    "CREATE TRIGGER main.{} AFTER {event} ON {} BEGIN {body} END",
                    quote(&trigger),
                    quote(&s.name)
                ),
            )?;
            objects.push(("trigger", trigger));
        }
    }
    Ok(objects)
}
