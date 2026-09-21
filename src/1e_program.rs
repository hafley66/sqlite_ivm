//! Every SQL string one view's drain can issue, built once per view and
//! connection, keyed by node id and role, so no drain call formats SQL.

use crate::{
    relational::{Kind, Occurrence, Plan, Rule},
    relational_materialize::MaterializeStatements,
    relational_maintenance::{
        arrived_table, columns, deleted_table, delta_index, delta_table, identity_sql, json_key,
        keys_table, left_table, out_table, parameters, plain, roles, rule_from, rule_where, table,
        Role,
    },
};
use rusqlite::{types::Value, Connection};

pub(crate) const CLEAR_TOUCHED: &str = "DELETE FROM temp.__ivm_touched";

pub(crate) struct Program {
    pub(crate) name: String,
    /// Bind-time DDL, one `execute_batch` per entry, in scratch creation order.
    pub(crate) scratch: Vec<String>,
    pub(crate) nodes: Vec<NodeStatements>,
    pub(crate) apply_state: ApplyStateStatements,
}

pub(crate) struct NodeStatements {
    pub(crate) sweep: String,
    pub(crate) kind: KindStatements,
}

pub(crate) enum KindStatements {
    Input {
        seed: String,
    },
    Map {
        materialize: MaterializeStatements,
    },
    SetAll {
        copies: Vec<String>,
    },
    Arrangement(ArrangementStatements),
    Fixpoint(FixpointStatements),
}

pub(crate) struct ArrangementStatements {
    pub(crate) park: String,
    pub(crate) clear_out: String,
    /// After-state minus before-state, executed in order.
    pub(crate) diff: [String; 2],
    pub(crate) consolidate: String,
    pub(crate) join_delta: Option<JoinDeltaStatements>,
    pub(crate) emit: String,
    pub(crate) clear_before: String,
    pub(crate) sides: Vec<Option<ArrangementSide>>,
    pub(crate) materialize: MaterializeStatements,
    pub(crate) stored_before: Option<StoredGroupStatements>,
    pub(crate) aggregate_delta: Option<AggregateDeltaStatements>,
}

pub(crate) struct StoredGroupStatements {
    pub(crate) read: String,
    pub(crate) corrupt: String,
}

pub(crate) struct AggregateDeltaStatements {
    pub(crate) eligible: String,
    pub(crate) apply: String,
    pub(crate) update_counts: String,
    pub(crate) invalidate: String,
}

/// Inner joins propagate signed input deltas against the opposite arrangement.
/// Side zero runs before its upsert; side one sees that updated side zero.
pub(crate) struct JoinDeltaStatements {
    pub(crate) sides: [String; 2],
    pub(crate) bad: String,
    pub(crate) consolidate: bool,
}

pub(crate) struct ArrangementSide {
    pub(crate) intern: String,
    pub(crate) touch: String,
    pub(crate) upsert: UpsertStatements,
}

pub(crate) struct UpsertStatements {
    pub(crate) clear_delta: String,
    pub(crate) fill_delta: String,
    pub(crate) apply: String,
    pub(crate) insert_new: Option<String>,
    pub(crate) bad: String,
    pub(crate) drop_zero: String,
}

pub(crate) struct SplitStatements {
    pub(crate) clear_arrived: String,
    pub(crate) clear_left: String,
    pub(crate) fill_left: String,
    pub(crate) fill_arrived: String,
}

pub(crate) struct FixpointStatements {
    pub(crate) max_all: String,
    pub(crate) max_work: String,
    pub(crate) clear_work: String,
    pub(crate) clear_deleted: String,
    pub(crate) collect_deleted: String,
    pub(crate) drop_deleted_range: String,
    /// Per member rule: the rederivability test over the work rows.
    pub(crate) restore: String,
    /// Per member rule, in rule order: closure derives over the whole member
    /// table bounded by a rowid range carried in `?1` and `?2`.
    pub(crate) round_derives: Vec<String>,
    pub(crate) delete_derives: Vec<String>,
    pub(crate) retract_gone: String,
    pub(crate) emit_stored: String,
    pub(crate) emit_fresh: String,
    pub(crate) seed_new: String,
    pub(crate) sides: Vec<Option<FixpointSide>>,
}

pub(crate) struct FixpointSide {
    pub(crate) intern: String,
    pub(crate) touch: String,
    pub(crate) upsert: UpsertStatements,
    pub(crate) split: SplitStatements,
    pub(crate) exists_left: String,
    /// Per rule mentioning the side, per occurrence of it: the derive reading
    /// that occurrence from the arrived delta.
    pub(crate) arrive_derives: Vec<Vec<String>>,
    pub(crate) left_derives: Vec<Vec<String>>,
}

pub(crate) struct ApplyStateStatements {
    pub(crate) wanted: String,
    pub(crate) retract: String,
    pub(crate) peak: String,
    pub(crate) extend: String,
}

impl Program {
    pub(crate) fn build(plan: &Plan, name: &str, db: &Connection) -> Program {
        Program {
            name: name.to_string(),
            scratch: scratch_statements(plan),
            nodes: (0..plan.nodes.len()).map(|id| node_statements(plan, name, db, id)).collect(),
            apply_state: apply_state_statements(plan, name),
        }
    }
}

fn scratch_statements(plan: &Plan) -> Vec<String> {
    let mut statements = vec![String::from(
        "CREATE TEMP TABLE IF NOT EXISTS __ivm_touched(__k INTEGER PRIMARY KEY)",
    )];
    for (id, node) in plan.nodes.iter().enumerate() {
        let width = node.fields.len();
        let cols = columns(width);
        statements.push(format!(
            "CREATE TABLE IF NOT EXISTS {out}({cols},__m); CREATE TABLE IF NOT EXISTS temp.__ivm_before_{width}_{id}({cols},__m)",
            out = out_table(id, width)
        ));
        if matches!(
            node.kind,
            Kind::Set(_) | Kind::Join { .. } | Kind::Group { .. } | Kind::Fixpoint { .. }
        ) {
            for side in 0..node.inputs.len() {
                let side_width = plan.nodes[node.inputs[side]].fields.len();
                statements.push(format!(
                    "CREATE TABLE IF NOT EXISTS {delta}(__r INTEGER NOT NULL,__v TEXT NOT NULL,__n INTEGER NOT NULL,{cols}); CREATE INDEX IF NOT EXISTS {index}_r ON {bare}(__r)",
                    delta = delta_table(id, side, side_width),
                    cols = columns(side_width),
                    index = delta_index(id, side, side_width),
                    bare = delta_table(id, side, side_width).trim_start_matches("temp.")
                ));
            }
        }
        if let Kind::Fixpoint { .. } = node.kind {
            for side in 0..node.inputs.len() {
                let side_width = plan.nodes[node.inputs[side]].fields.len();
                let side_cols = columns(side_width);
                statements.push(format!(
                    "CREATE TABLE IF NOT EXISTS {arrived}({side_cols}); CREATE TABLE IF NOT EXISTS {left}({side_cols})",
                    arrived = arrived_table(id, side, side_width),
                    left = left_table(id, side, side_width)
                ));
            }
            statements.push(format!(
                "CREATE TABLE IF NOT EXISTS {deleted}(__k TEXT PRIMARY KEY,{cols})",
                deleted = deleted_table(id, width)
            ));
        }
    }
    statements
}

fn node_statements(plan: &Plan, name: &str, db: &Connection, id: usize) -> NodeStatements {
    let node = &plan.nodes[id];
    let width = node.fields.len();
    let out = out_table(id, width);
    NodeStatements {
        sweep: format!("DELETE FROM {out}"),
        kind: match &node.kind {
            Kind::Input(source) => {
                let width = plan.sources[*source].columns.len();
                KindStatements::Input {
                    seed: format!("INSERT INTO {out} VALUES({})", parameters(width + 1)),
                }
            }
            Kind::Map { .. } => KindStatements::Map {
                materialize: plan.materialize_statements(db, name, id, false),
            },
            Kind::Set("all") => KindStatements::SetAll {
                copies: node
                    .inputs
                    .iter()
                    .map(|input| {
                        let child = out_table(*input, plan.nodes[*input].fields.len());
                        format!(
                            "INSERT INTO {out}({cols},__m) SELECT {cols},__m FROM {child}",
                            cols = columns(width)
                        )
                    })
                    .collect(),
            },
            Kind::Set(_) | Kind::Join { .. } | Kind::Group { .. } => {
                KindStatements::Arrangement(arrangement_statements(plan, name, db, id, width))
            }
            Kind::Fixpoint { .. } => {
                KindStatements::Fixpoint(fixpoint_statements(plan, name, db, id, width))
            }
        },
    }
}

fn arrangement_statements(
    plan: &Plan,
    name: &str,
    db: &Connection,
    id: usize,
    width: usize,
) -> ArrangementStatements {
    let node = &plan.nodes[id];
    let out = out_table(id, width);
    let cols = columns(width);
    let before = format!("temp.__ivm_before_{width}_{id}");
    let identity = identity_sql(width);
    ArrangementStatements {
        park: format!("INSERT INTO {before} SELECT * FROM {out}"),
        clear_out: format!("DELETE FROM {out}"),
        diff: [
            format!("INSERT INTO {out} SELECT {cols},-__m FROM {before}"),
            format!("DELETE FROM {before}"),
        ],
        consolidate: format!("INSERT INTO {before} SELECT {cols},sum(__m) FROM {out} GROUP BY {identity} HAVING sum(__m)!=0"),
        join_delta: join_delta_statements(plan, name, id),
        emit: format!("INSERT INTO {out} SELECT * FROM {before}"),
        clear_before: format!("DELETE FROM {before}"),
        sides: (0..node.inputs.len())
            .map(|side| arrangement_side(plan, name, db, id, side))
            .collect(),
        materialize: plan.materialize_statements(db, name, id, true),
        aggregate_delta: aggregate_delta_statements(plan, name, db, id),
        stored_before: if id == plan.output {
            plan.stored_group_key().and_then(|key| {
                let state_name = format!("{name}_state");
                let suffix = format!("({key})");
                let indexed = crate::statements::query_map(
                    db, crate::statements::Phase::Declare, name,
                    "SELECT sql FROM main.sqlite_schema WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL",
                    [&state_name], |row| row.get::<_, String>(0),
                ).is_ok_and(|definitions| definitions.iter().any(|sql| sql.ends_with(&suffix)));
                indexed.then(|| {
                    let selected = format!("FROM main.{} WHERE {key} IN (SELECT __v FROM {} WHERE __i IN (SELECT __k FROM temp.__ivm_touched))", crate::catalog::quote(&state_name), keys_table(name));
                    StoredGroupStatements {
                        read: format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 {selected}"),
                        corrupt: format!("SELECT EXISTS(SELECT 1 {selected} AND __key!={identity})"),
                    }
                })
            })
        } else { None },
    }
}

fn join_delta_statements(plan: &Plan, name: &str, id: usize) -> Option<JoinDeltaStatements> {
    let node = &plan.nodes[id];
    let Kind::Join { mode: "inner", predicate, .. } = &node.kind else {
        return None;
    };
    let left_width = plan.nodes[node.inputs[0]].fields.len();
    let right_width = plan.nodes[node.inputs[1]].fields.len();
    let cols = columns(node.fields.len());
    let out = out_table(id, node.fields.len());
    let dict = keys_table(name);
    let projection = (0..left_width).map(|i| format!("l.c{i} AS c{i}"))
        .chain((0..right_width).map(|i| format!("r.c{i} AS c{}", left_width + i)))
        .collect::<Vec<_>>().join(",");
    let sides = [0, 1].map(|side| {
        let child = out_table(node.inputs[side], plan.nodes[node.inputs[side]].fields.len());
        let key = plan.key_sql(id, side).expect("inner join input key");
        let delta = format!("(SELECT *, (SELECT __i FROM {dict} WHERE __v={key}) AS __k FROM {child})");
        let (left, right, weight) = if side == 0 {
            (delta, table(name, id, 1), "l.__m*r.__n")
        } else {
            (table(name, id, 0), delta, "l.__n*r.__m")
        };
        let body = format!("SELECT {projection},{weight} AS __m FROM {left} l JOIN {right} r ON l.__k=r.__k AND NOT EXISTS(SELECT 1 FROM json_each((SELECT __v FROM {dict} WHERE __i=l.__k)) WHERE type='null')");
        let filter = predicate.as_ref().map(|p| format!(" WHERE {p}")).unwrap_or_default();
        format!("INSERT INTO {out}({cols},__m) SELECT * FROM ({body}){filter}")
    });
    Some(JoinDeltaStatements {
        sides,
        bad: format!("SELECT EXISTS(SELECT 1 FROM {out} WHERE typeof(__m)!='integer')"),
        consolidate: !group_consumers_consolidate(plan, id),
    })
}

/// A group consolidates its input before applying multiplicities. Plain column
/// projections between the join and group preserve signed bags and cannot raise
/// a scalar error on a row that would otherwise cancel at the join.
fn group_consumers_consolidate(plan: &Plan, id: usize) -> bool {
    if id == plan.output {
        return false;
    }
    let consumers = plan.nodes.iter().enumerate()
        .filter(|(_, node)| node.inputs.contains(&id)).collect::<Vec<_>>();
    !consumers.is_empty() && consumers.into_iter().all(|(consumer, node)| match &node.kind {
        Kind::Group { .. } => true,
        Kind::Map { expressions, predicate: None } if expressions.iter().all(|expression| {
            expression.strip_prefix('c').is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        }) => group_consumers_consolidate(plan, consumer),
        _ => false,
    })
}

/// Recover the scalar input of the compiler's weighted SUM through the same
/// SQLite parser. This accepts its generated CASE/weight form only.
fn weighted_sum_value(expression: &str) -> Option<String> {
    use sqlite3_parser::{ast::*, lexer::sql::Parser, Bump, FallibleIterator};
    fn unwrap<'a>(value: &'a Expr<'a>) -> &'a Expr<'a> {
        match value {
            Expr::Parenthesized(values) if values.len() == 1 => unwrap(&values[0]),
            _ => value,
        }
    }
    let arena = Bump::new();
    let query = format!("SELECT {expression}");
    let mut parser = Parser::new(&arena, query.as_bytes());
    let Cmd::Stmt(Stmt::Select(select)) = parser.next().ok()?? else { return None };
    let OneSelect::Select { columns, .. } = &select.body.select else { return None };
    let ResultColumn::Expr(Expr::FunctionCall { name, args: Some(args), distinctness: None, filter_over: None, order_by: None }, _) = &columns[0] else { return None };
    if !name.0.eq_ignore_ascii_case("sum") || args.len() != 1 { return None; }
    let Expr::Case { else_expr: Some(weighted), .. } = unwrap(&args[0]) else { return None };
    let Expr::Binary(value, Operator::Multiply, weight) = unwrap(weighted) else { return None };
    if crate::relational::sql(unwrap(weight)) != "__n" { return None; }
    Some(crate::relational::sql(unwrap(value)))
}

/// Classify the existing compiler output, leaving all other aggregate shapes
/// on their general arrangement path.
pub(crate) fn aggregate_sum_inputs(plan: &Plan) -> Option<Vec<(usize, String)>> {
    let node = &plan.nodes[plan.output];
    let Kind::Group { keys, expressions, window: false, limit: None, having: None, .. } = &node.kind else { return None };
    plan.stored_group_key()?;
    let key_positions = keys.iter().map(|key| expressions.iter().position(|e| e == key)).collect::<Option<Vec<_>>>()?;
    if key_positions.iter().any(|i| node.fields[*i].affinity != "INTEGER") { return None; }
    expressions.iter().position(|e| e == "coalesce(sum(__n),0)")?;
    let mut sums = vec![];
    for (i, expression) in expressions.iter().enumerate() {
        if key_positions.contains(&i) || expression == "coalesce(sum(__n),0)" { continue; }
        sums.push((i, weighted_sum_value(expression)?));
    }
    (!sums.is_empty()).then_some(sums)
}

/// Safe integer groups add signed contributions to their stored before-image.
/// Nullable sums carry non-null support counts. Groups leaving the bounded
/// domain use the authoritative input arrangement for subsequent recomputations.
fn aggregate_delta_statements(plan: &Plan, name: &str, db: &Connection, id: usize) -> Option<AggregateDeltaStatements> {
    if id != plan.output { return None; }
    let sums = aggregate_sum_inputs(plan)?;
    let node = &plan.nodes[id];
    let Kind::Group { keys, expressions, .. } = &node.kind else { return None };
    let key_positions = keys.iter().map(|key| expressions.iter().position(|e| e == key)).collect::<Option<Vec<_>>>()?;
    let count = expressions.iter().position(|e| e == "coalesce(sum(__n),0)")?;
    let width = node.fields.len();
    let cols = columns(width);
    let out = out_table(id, width);
    let before = format!("temp.__ivm_before_{width}_{id}");
    let child = out_table(node.inputs[0], plan.nodes[node.inputs[0]].fields.len());
    let metadata = table(name, id, 1);
    let metadata_name = format!("{name}_op{id}x1");
    let present = db.query_row("SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1)", [&metadata_name], |row| row.get::<_, bool>(0)).unwrap_or(false);
    if !present { return None; }
    let delta = format!("(SELECT *,__m AS __n FROM {child})");
    let mut checks = vec![
        format!("NOT EXISTS(SELECT 1 FROM {metadata} WHERE __k IN (SELECT __k FROM temp.__ivm_touched) AND __safe=0)"),
        format!("(SELECT coalesce(max(c{count}),0) FROM {before})+(SELECT total(abs(CAST(__n AS REAL))) FROM {delta})<=1000000"),
    ];
    for (_, value) in &sums {
        checks.push(format!("(SELECT coalesce(min(typeof({value}) IN ('integer','null') AND coalesce(abs(CAST(({value}) AS REAL)),0)<=1000000),1) FROM {delta})"));
    }
    let new_count = format!("coalesce(b.c{count},0)+g.c{count}");
    let values = expressions.iter().enumerate().map(|(i, expression)| {
        if key_positions.contains(&i) { format!("g.c{i}") }
        else if expression == "coalesce(sum(__n),0)" { new_count.clone() }
        else { format!("CASE WHEN m.nn{i}=0 THEN NULL ELSE coalesce(b.c{i},0)+coalesce(g.c{i},0) END") }
    }).collect::<Vec<_>>().join(",");
    let groups = if keys.is_empty() { String::new() } else { format!(" GROUP BY {}", plan.key_sql(id, 0)?) };
    let joined = if key_positions.is_empty() { "1".to_string() } else {
        key_positions.iter().map(|i| format!("g.c{i} IS b.c{i}")).collect::<Vec<_>>().join(" AND ")
    };
    let nonempty = if keys.is_empty() { String::new() } else { format!(" WHERE ({new_count})>0") };
    let projected = expressions.iter().enumerate().map(|(i, e)|format!("{e} AS c{i}")).collect::<Vec<_>>().join(",");
    let key = plan.key_sql(id, 0)?;
    let dict = keys_table(name);
    let metadata_columns = sums.iter().map(|(i,_)|format!("nn{i}")).collect::<Vec<_>>().join(",");
    let nonnull = sums.iter().map(|(_,v)|format!("sum(CASE WHEN ({v}) IS NULL THEN 0 ELSE __n END)")).collect::<Vec<_>>().join(",");
    let updates = sums.iter().map(|(i,_)|format!("nn{i}=nn{i}+excluded.nn{i}")).collect::<Vec<_>>().join(",");
    Some(AggregateDeltaStatements {
        eligible: format!("SELECT {}", checks.join(" AND ")),
        update_counts: format!("INSERT INTO {metadata}(__k,__safe,{metadata_columns}) SELECT (SELECT __i FROM {dict} WHERE __v={key}),1,{nonnull} FROM {delta} WHERE true GROUP BY {key} ON CONFLICT(__k) DO UPDATE SET {updates}"),
        invalidate: format!("INSERT INTO {metadata}(__k,__safe,{metadata_columns}) SELECT __k,0,{} FROM temp.__ivm_touched WHERE true ON CONFLICT(__k) DO UPDATE SET __safe=0", sums.iter().map(|_|"0").collect::<Vec<_>>().join(",")),
        apply: format!("INSERT INTO {out}({cols},__m) SELECT {values},1 FROM (SELECT {projected},(SELECT __i FROM {dict} WHERE __v={key}) AS __group_key FROM {delta}{groups}) g LEFT JOIN {before} b ON {joined} JOIN {metadata} m ON m.__k=g.__group_key{nonempty}"),
    })
}

fn arrangement_side(plan: &Plan, name: &str, db: &Connection, id: usize, side: usize) -> Option<ArrangementSide> {
    let node = &plan.nodes[id];
    let child = out_table(node.inputs[side], plan.nodes[node.inputs[side]].fields.len());
    let dict = keys_table(name);
    let key = plan.key_sql(id, side)?;
    Some(ArrangementSide {
        intern: format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {key} FROM {child}"),
        touch: format!("INSERT OR IGNORE INTO temp.__ivm_touched SELECT __i FROM {dict} WHERE __v IN (SELECT {key} FROM {child})"),
        upsert: upsert_statements(plan, name, db, id, side, &key),
    })
}

fn upsert_statements(
    plan: &Plan,
    name: &str,
    db: &Connection,
    id: usize,
    side: usize,
    key: &str,
) -> UpsertStatements {
    let node = &plan.nodes[id];
    let child_width = plan.nodes[node.inputs[side]].fields.len();
    let child = out_table(node.inputs[side], child_width);
    let t = table(name, id, side);
    let cols = columns(child_width);
    let identity = identity_sql(child_width);
    let identity_of =
        |alias: &str| json_key((0..child_width).map(|i| plain(&format!("{alias}.c{i}"))).collect());
    let dict = keys_table(name);
    let delta = delta_table(id, side, child_width);
    let arrangement_identity = identity_of(&t);
    let table_name = format!("{name}_op{id}x{side}");
    let suffix = format!("(__r,{identity})");
    let indexed_identity = crate::statements::query_map(
        db, crate::statements::Phase::Declare, name,
        "SELECT sql FROM main.sqlite_schema WHERE type='index' AND tbl_name=?1 AND sql LIKE 'CREATE UNIQUE INDEX%'",
        [&table_name], |row| row.get::<_, String>(0),
    ).is_ok_and(|definitions| definitions.iter().any(|sql| sql.ends_with(&suffix)));
    let insert = format!("INSERT INTO {t}(__k,__r,__n,{cols}) SELECT (SELECT __i FROM {dict} WHERE __v={key}),d.__r,d.__n,{cols} FROM {delta} d");
    UpsertStatements {
        clear_delta: format!("DELETE FROM {delta}"),
        fill_delta: format!(
            "INSERT INTO {delta}(__r,__v,__n,{cols}) SELECT sqlite_ivm_hash(__ivm_v),__ivm_v,__ivm_n,{cols} \
             FROM (SELECT {identity} AS __ivm_v,sum(__m) AS __ivm_n,{cols} FROM {child} GROUP BY {identity}) WHERE __ivm_n!=0"
        ),
        apply: if indexed_identity {
            format!("{insert} WHERE true ON CONFLICT(__r,{identity}) DO UPDATE SET __n={t}.__n+excluded.__n")
        } else {
            format!("UPDATE {t} SET __n={t}.__n+d.__n FROM {delta} d WHERE {t}.__r IN (SELECT __r FROM {delta}) AND d.__r={t}.__r AND d.__v={arrangement_identity}")
        },
        insert_new: (!indexed_identity).then(|| format!(
            "{insert} WHERE NOT EXISTS(SELECT 1 FROM {t} a WHERE a.__r=d.__r AND {}=d.__v)", identity_of("a")
        )),
        // Only identities present in this delta can have changed multiplicity.
        // The hash bounds the index lookup; the update above still checks the
        // full identity so collisions cannot apply another row's delta.
        bad: format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE __r IN (SELECT __r FROM {delta}) AND (__n<0 OR typeof(__n)!='integer'))"),
        drop_zero: format!("DELETE FROM {t} WHERE __r IN (SELECT __r FROM {delta}) AND __n=0"),
    }
}

fn split_statements(plan: &Plan, name: &str, id: usize, side: usize) -> SplitStatements {
    let node = &plan.nodes[id];
    let width = plan.nodes[node.inputs[side]].fields.len();
    let child = out_table(node.inputs[side], width);
    let t = table(name, id, side);
    let arrived = arrived_table(id, side, width);
    let left = left_table(id, side, width);
    let cols = columns(width);
    let identity_of =
        |alias: &str| json_key((0..width).map(|i| plain(&format!("{alias}.c{i}"))).collect());
    let delta_identity = identity_of("o");
    let stored_identity = identity_of("a");
    let net = format!(
        "(SELECT sum(o.__m) FROM {child} o WHERE sqlite_ivm_hash({delta_identity})=a.__r AND {delta_identity}={stored_identity})"
    );
    SplitStatements {
        clear_arrived: format!("DELETE FROM {arrived}"),
        clear_left: format!("DELETE FROM {left}"),
        fill_left: format!(
            "INSERT INTO {left} SELECT {cols} FROM {t} a \
             WHERE a.__r IN (SELECT sqlite_ivm_hash({delta_identity}) FROM {child} o) AND a.__n+coalesce({net},0)=0"
        ),
        fill_arrived: format!(
            "INSERT INTO {arrived} SELECT {cols} FROM (SELECT {identity} AS __ivm_r,sum(__m) AS __ivm_n,{cols} FROM {child} GROUP BY {identity}) o \
             WHERE __ivm_n>0 AND NOT EXISTS(SELECT 1 FROM {t} a WHERE a.__r=sqlite_ivm_hash(o.__ivm_r) AND {stored_identity}=o.__ivm_r)",
            identity = identity_sql(width)
        ),
    }
}

fn fixpoint_statements(plan: &Plan, name: &str, db: &Connection, id: usize, width: usize) -> FixpointStatements {
    let node = &plan.nodes[id];
    let Kind::Fixpoint { rules } = &node.kind else {
        unreachable!()
    };
    let member = node.inputs.len();
    let all = table(name, id, member);
    let work = table(name, id, member + 1);
    let cols = columns(width);
    let out = out_table(id, width);
    let deleted = deleted_table(id, width);
    let identity_of =
        |alias: &str| json_key((0..width).map(|i| plain(&format!("{alias}.c{i}"))).collect());
    let d_identity = identity_of("d");
    let a_identity = identity_of("a");
    let d_cols = (0..width).map(|i| format!("d.c{i}")).collect::<Vec<_>>().join(",");
    let a_cols = (0..width).map(|i| format!("a.c{i}")).collect::<Vec<_>>().join(",");
    let matched = |rule: &Rule| {
        rule.head
            .iter()
            .zip(&node.fields)
            .enumerate()
            .map(|(i, (h, f))| format!("(({h}) COLLATE {}) IS w.c{i}", f.collation))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let plain_from = |rule: &Rule| {
        let mut params: Vec<Value> = vec![];
        rule_from(
            rule,
            &roles(name, id, 0, rule, None, Role::Table(all.clone())),
            &mut params,
        )
    };
    let restore_parts = rules
        .iter()
        .map(|rule| {
            format!(
                "EXISTS(SELECT 1 {}{})",
                plain_from(rule),
                rule_where(rule, Some(&matched(rule)))
            )
        })
        .collect::<Vec<_>>();
    FixpointStatements {
        max_all: format!("SELECT coalesce(max(rowid),0) FROM {all}"),
        max_work: format!("SELECT coalesce(max(rowid),0) FROM {work}"),
        clear_work: format!("DELETE FROM {work}"),
        clear_deleted: format!("DELETE FROM {deleted}"),
        collect_deleted: format!(
            "INSERT OR IGNORE INTO {deleted}(__k,{cols}) SELECT __k,{cols} FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"
        ),
        drop_deleted_range: format!(
            "DELETE FROM {all} WHERE __k IN (SELECT __k FROM {work} WHERE rowid>?1 AND rowid<=?2)"
        ),
        restore: format!(
            "INSERT OR IGNORE INTO {all}(__k,{cols}) SELECT w.__k,{} FROM {work} w WHERE {}",
            (0..width).map(|i| format!("w.c{i}")).collect::<Vec<_>>().join(","),
            restore_parts.join(" OR ")
        ),
        round_derives: rules
            .iter()
            .filter(|r| r.member().is_some())
            .map(|rule| {
                let mut params: Vec<Value> = vec![];
                let from = rule_from(
                    rule,
                    &roles(name, id, 0, rule, None, Role::Range(all.clone(), 0, 0)),
                    &mut params,
                );
                derive_sql(&all, &all, &cols, rule, &from, false)
            })
            .collect(),
        delete_derives: rules
            .iter()
            .filter(|r| r.member().is_some())
            .map(|rule| {
                let mut params: Vec<Value> = vec![];
                let from = rule_from(
                    rule,
                    &roles(name, id, 0, rule, None, Role::Range(work.clone(), 0, 0)),
                    &mut params,
                );
                derive_sql(&work, &all, &cols, rule, &from, true)
            })
            .collect(),
        retract_gone: format!(
            "INSERT INTO {out}({cols},__m) SELECT {d_cols},-1 FROM {deleted} d \
             WHERE NOT EXISTS(SELECT 1 FROM {all} a WHERE a.__k=d.__k AND {a_identity}={d_identity})"
        ),
        emit_stored: format!(
            "INSERT INTO {out}({cols},__m) SELECT {a_cols},1 FROM {deleted} d JOIN {all} a ON a.__k=d.__k \
             WHERE {a_identity}!={d_identity}"
        ),
        emit_fresh: format!(
            "INSERT INTO {out}({cols},__m) SELECT {a_cols},1 FROM {all} a WHERE a.rowid>?1 \
             AND NOT EXISTS(SELECT 1 FROM {deleted} d WHERE d.__k=a.__k)"
        ),
        seed_new: format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 FROM {all} WHERE rowid>?1"),
        sides: (0..node.inputs.len())
            .map(|side| fixpoint_side(plan, name, db, id, side, &all, &work))
            .collect(),
    }
}

fn fixpoint_side(
    plan: &Plan,
    name: &str,
    db: &Connection,
    id: usize,
    side: usize,
    all: &str,
    work: &str,
) -> Option<FixpointSide> {
    let node = &plan.nodes[id];
    let Kind::Fixpoint { rules } = &node.kind else {
        unreachable!()
    };
    let child_width = plan.nodes[node.inputs[side]].fields.len();
    let child = out_table(node.inputs[side], child_width);
    let dict = keys_table(name);
    let key = plan.key_sql(id, side)?;
    let arrived = arrived_table(id, side, child_width);
    let left = left_table(id, side, child_width);
    let cols = columns(plan.nodes[id].fields.len());
    let side_derives = |delta: &str| {
        rules
            .iter()
            .filter(|r| r.mentions(side))
            .map(|rule| {
                rule.occurrences
                    .iter()
                    .enumerate()
                    .filter(|(_, (o, _))| *o == Occurrence::Input(side))
                    .map(|(at, _)| {
                        let mut params: Vec<Value> = vec![];
                        let from = rule_from(
                            rule,
                            &roles(
                                name,
                                id,
                                side,
                                rule,
                                Some((at, delta)),
                                Role::Table(all.to_string()),
                            ),
                            &mut params,
                        );
                        derive_sql(all, all, &cols, rule, &from, false)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    Some(FixpointSide {
        intern: format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {key} FROM {child}"),
        touch: format!("INSERT OR IGNORE INTO temp.__ivm_touched SELECT __i FROM {dict} WHERE __v IN (SELECT {key} FROM {child})"),
        upsert: upsert_statements(plan, name, db, id, side, &key),
        split: split_statements(plan, name, id, side),
        exists_left: format!("SELECT EXISTS(SELECT 1 FROM {left})"),
        arrive_derives: side_derives(&arrived),
        left_derives: rules
            .iter()
            .filter(|r| r.mentions(side))
            .map(|rule| {
                rule.occurrences
                    .iter()
                    .enumerate()
                    .filter(|(_, (o, _))| *o == Occurrence::Input(side))
                    .map(|(at, _)| {
                        let mut params: Vec<Value> = vec![];
                        let from = rule_from(
                            rule,
                            &roles(
                                name,
                                id,
                                side,
                                rule,
                                Some((at, &left)),
                                Role::Table(all.to_string()),
                            ),
                            &mut params,
                        );
                        derive_sql(work, all, &cols, rule, &from, true)
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    })
}

/// The drain's derive: the only-present shape filters members already in the
/// set, the plain shape trusts the caller's range.
fn derive_sql(
    target: &str,
    members: &str,
    cols: &str,
    rule: &Rule,
    from: &str,
    only_present: bool,
) -> String {
    if only_present {
        format!(
            "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT __k,{cols} FROM (SELECT {} AS __k,{} {from}{}) WHERE __k IN (SELECT __k FROM {members})",
            rule.key,
            rule.head
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{h} AS c{i}"))
                .collect::<Vec<_>>()
                .join(","),
            rule_where(rule, None)
        )
    } else {
        format!(
            "INSERT OR IGNORE INTO {target}(__k,{cols}) SELECT {},{} {from}{}",
            rule.key,
            rule.head.join(","),
            rule_where(rule, None)
        )
    }
}

fn apply_state_statements(plan: &Plan, name: &str) -> ApplyStateStatements {
    let node = &plan.nodes[plan.output];
    let width = node.fields.len();
    let out = out_table(plan.output, width);
    let state = format!("main.{}", crate::catalog::quote(&format!("{name}_state")));
    let key = json_key((0..width).map(|i| plain(&format!("o.c{i}"))).collect());
    ApplyStateStatements {
        wanted: format!("SELECT coalesce(sum(-__m),0) FROM {out} o WHERE __m<0"),
        retract: format!(
            "DELETE FROM {state} WHERE rowid IN (SELECT s.rowid FROM {state} s JOIN (SELECT {key} AS __key,sum(-__m) AS __n FROM {out} o WHERE __m<0 GROUP BY {key}) d ON s.__key=d.__key \
             WHERE (SELECT count(*) FROM {state} p WHERE p.__key=s.__key AND p.rowid<=s.rowid)<=d.__n)"
        ),
        peak: format!("SELECT coalesce(max(__m),0) FROM {out}"),
        extend: format!(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce(max(__m),0) FROM {out})) \
             INSERT INTO {state}(__key,{}) SELECT {key},o.c0{} FROM {out} o,seq WHERE o.__m>0 AND seq.n<=o.__m",
            columns(width),
            (1..width).map(|i| format!(",o.c{i}")).collect::<String>()
        ),
    }
}
