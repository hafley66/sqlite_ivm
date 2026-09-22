//! Every SQL string one view's drain can issue, built once per view and
//! connection, keyed by node id and role, so no drain call formats SQL.

use crate::{
    relational::{Kind, Occurrence, Plan, Rule},
    relational_materialize::MaterializeStatements,
    relational_maintenance::{
        arrived_table, columns, deleted_table, identity_sql, json_key,
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
    pub(crate) materialize_before: MaterializeStatements,
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

/// Inner joins probe current sources and subtract the two-delta cross term.
pub(crate) struct JoinDeltaStatements {
    pub(crate) sides: [String; 2],
    pub(crate) cross: String,
    pub(crate) bad: String,
    pub(crate) consolidate: bool,
}

pub(crate) struct ArrangementSide {
    pub(crate) intern: String,
    pub(crate) touch: String,
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
    pub(crate) split: SplitStatements,
    pub(crate) exists_left: String,
    /// Per rule mentioning the side, per occurrence of it: the derive reading
    /// that occurrence from the arrived delta.
    pub(crate) arrive_derives: Vec<Vec<String>>,
    pub(crate) left_derives: Vec<Vec<String>>,
}

pub(crate) struct ApplyStateStatements {
    pub(crate) wanted: String,
    pub(crate) replace: Option<String>,
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
        if id == plan.output {
            if let Some(keys) = plan.stored_group_key().filter(|k| !k.is_empty()) {
                let signature = crate::relational_maintenance::row_hash(keys.as_bytes()) as u64;
                statements.push(format!("CREATE INDEX IF NOT EXISTS temp.__ivm_out_{width}_{id}_group_{signature:x} ON __ivm_out_{width}_{id}({keys},__m)"));
            }
        }
        // Aggregate deltas join the stored before-image by projected group
        // columns. The scratch table otherwise requires one scan per group.
        if id == plan.output && aggregate_sum_inputs(plan).is_some() {
            if let Kind::Group { keys, expressions, .. } = &node.kind {
                let columns = keys.iter().map(|key| {
                    format!("c{}", expressions.iter().position(|e| e == key).expect("projected group key"))
                }).collect::<Vec<_>>();
                if !columns.is_empty() {
                    statements.push(format!(
                        "CREATE INDEX IF NOT EXISTS temp.__ivm_before_{width}_{id}_group_{} ON __ivm_before_{width}_{id}({})",
                        columns.join("_"), columns.join(",")
                    ));
                }
            }
        }
        if matches!(node.kind,Kind::Join { .. } | Kind::Group { .. }) {
            for (side, input) in node.inputs.iter().enumerate() {
                let child_width = plan.nodes[*input].fields.len();
                let child = out_table(*input, child_width);
                let key = plan.native_key_columns(id,side).expect("join key").join(",");
                if key.is_empty() { continue; }
                let signature = crate::relational_maintenance::row_hash(key.as_bytes()) as u64;
                statements.push(format!("CREATE INDEX IF NOT EXISTS temp.__ivm_out_{child_width}_{input}_key_{signature:x} ON {}({key})",child.trim_start_matches("temp.")));
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
    ArrangementStatements {
        park: format!("INSERT INTO {before} SELECT * FROM {out}"),
        clear_out: format!("DELETE FROM {out}"),
        diff: [
            format!("INSERT INTO {out} SELECT {cols},-__m FROM {before}"),
            format!("DELETE FROM {before}"),
        ],
        consolidate: format!("INSERT INTO {before} SELECT {cols},sum(__m) FROM {out} GROUP BY {} HAVING sum(__m)!=0", crate::native_keys::exact_row_columns(width,"").join(",")),
        join_delta: join_delta_statements(plan, name, db, id),
        emit: format!("INSERT INTO {out} SELECT * FROM {before}"),
        clear_before: format!("DELETE FROM {before}"),
        sides: (0..node.inputs.len())
            .map(|side| arrangement_side(plan, name, id, side))
            .collect(),
        materialize: plan.materialize_statements(db, name, id, true),
        materialize_before: plan.materialize_from_sources(db, name, id, true, Some(
            &(0..node.inputs.len()).map(|side| plan.live_input(db, name, id, side, true, true)).collect::<Vec<_>>()
        )),
        aggregate_delta: aggregate_delta_statements(plan, name, db, id),
        stored_before: if id == plan.output {
            plan.stored_group_key().and_then(|key| {
                let keys = if key.is_empty() { vec!["0".into()] } else { key.split(',').map(str::to_string).collect() };
                let source = format!("main.{}",crate::catalog::quote(&format!("{name}_state")));
                let selected = plan.touched_source(name,id,&keys,&source);
                let projection = (0..width).map(|i|format!("s.c{i}")).collect::<Vec<_>>().join(",");
                let check = format!("sqlite_ivm_row_check({projection})");
                Some(StoredGroupStatements {
                    read: format!("INSERT INTO {out}({cols},__m) SELECT {projection},1 FROM {selected}"),
                    corrupt: format!("SELECT EXISTS(SELECT 1 FROM {selected} WHERE s.__check!={check})"),
                })
            })
        } else { None },
    }
}

fn join_delta_statements(plan: &Plan, name: &str, db: &Connection, id: usize) -> Option<JoinDeltaStatements> {
    let node = &plan.nodes[id];
    let Kind::Join { mode: "inner", predicate, .. } = &node.kind else {
        return None;
    };
    let left_width = plan.nodes[node.inputs[0]].fields.len();
    let right_width = plan.nodes[node.inputs[1]].fields.len();
    let cols = columns(node.fields.len());
    let out = out_table(id, node.fields.len());
    let projection = (0..left_width).map(|i| format!("l.c{i} AS c{i}"))
        .chain((0..right_width).map(|i| format!("r.c{i} AS c{}", left_width + i)))
        .collect::<Vec<_>>().join(",");
    let delta = |side: usize| {
        let child = out_table(node.inputs[side], plan.nodes[node.inputs[side]].fields.len());
        child
    };
    let matched = plan.native_join_match(id);
    let join = |left: String, right: String, weight: &str, drive_right: bool| {
        let from = if drive_right { format!("{right} r CROSS JOIN {left} l") } else { format!("{left} l CROSS JOIN {right} r") };
        let body = format!("SELECT {projection},{weight} AS __m FROM {from} ON {matched}");
        let filter = predicate.as_ref().map(|p| format!(" WHERE {p}")).unwrap_or_default();
        format!("INSERT INTO {out}({cols},__m) SELECT * FROM ({body}){filter}")
    };
    let sides = [
        join(delta(0), plan.live_input(db,name,id,1,false,false), "l.__m*r.__n", false),
        join(plan.live_input(db,name,id,0,false,false), delta(1), "l.__n*r.__m", true),
    ];
    Some(JoinDeltaStatements {
        sides,
        cross: join(delta(0), delta(1), "-l.__m*r.__m", false),
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
/// domain recompute affected groups from authoritative source reads.
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
    let groups = if keys.is_empty() { String::new() } else { format!(" GROUP BY {}", keys.join(",")) };
    let joined = if key_positions.is_empty() { "1".to_string() } else {
        // Both values are projected integer group keys. Remove the computed
        // expression's affinity so SQLite can probe the untyped scratch index.
        key_positions.iter().map(|i| format!("+g.c{i} IS b.c{i}")).collect::<Vec<_>>().join(" AND ")
    };
    let nonempty = if keys.is_empty() { String::new() } else { format!(" WHERE ({new_count})>0") };
    let projected = expressions.iter().enumerate().map(|(i, e)|format!("{e} AS c{i}")).collect::<Vec<_>>().join(",");
    let key = plan.key_lookup(name,id,0);
    let grouped = if keys.is_empty() { "(0+0)".into() } else { keys.join(",") };
    let metadata_columns = sums.iter().map(|(i,_)|format!("nn{i}")).collect::<Vec<_>>().join(",");
    let nonnull = sums.iter().map(|(_,v)|format!("sum(CASE WHEN ({v}) IS NULL THEN 0 ELSE __n END)")).collect::<Vec<_>>().join(",");
    let updates = sums.iter().map(|(i,_)|format!("nn{i}=nn{i}+excluded.nn{i}")).collect::<Vec<_>>().join(",");
    Some(AggregateDeltaStatements {
        eligible: format!("SELECT {}", checks.join(" AND ")),
        update_counts: format!("INSERT INTO {metadata}(__k,__safe,{metadata_columns}) SELECT {key},1,{nonnull} FROM {delta} WHERE true GROUP BY {grouped} ON CONFLICT(__k) DO UPDATE SET {updates}"),
        invalidate: format!("INSERT INTO {metadata}(__k,__safe,{metadata_columns}) SELECT __k,0,{} FROM temp.__ivm_touched WHERE true ON CONFLICT(__k) DO UPDATE SET __safe=0", sums.iter().map(|_|"0").collect::<Vec<_>>().join(",")),
        apply: format!("INSERT INTO {out}({cols},__m) SELECT {values},1 FROM (SELECT {projected},{key} AS __group_key FROM {delta}{groups}) g LEFT JOIN {before} b ON {joined} JOIN {metadata} m ON m.__k=g.__group_key{nonempty}"),
    })
}

fn arrangement_side(plan: &Plan, name: &str, id: usize, side: usize) -> Option<ArrangementSide> {
    let node = &plan.nodes[id];
    let child = out_table(node.inputs[side], plan.nodes[node.inputs[side]].fields.len());
    let key = if plan.native_key_values(id,side).is_some() {
        plan.key_lookup(name,id,side)
    } else {
        let dict = keys_table(name);
        format!("(SELECT __i FROM {dict} WHERE __v={})",plan.key_sql(id,side)?)
    };
    Some(ArrangementSide {
        intern: plan.insert_keys(name,id,side),
        touch: format!("INSERT OR IGNORE INTO temp.__ivm_touched SELECT {key} FROM {child}"),
    })
}

fn split_statements(plan: &Plan, name: &str, db: &Connection, id: usize, side: usize) -> SplitStatements {
    let node = &plan.nodes[id];
    let width = plan.nodes[node.inputs[side]].fields.len();
    let child = out_table(node.inputs[side], width);
    let t = plan.live_input(db,name,id,side,false,false);
    let arrived = arrived_table(id, side, width);
    let left = left_table(id, side, width);
    let cols = columns(width);
    let identity = identity_sql(width);
    let delta = format!("(SELECT {identity} AS __v,{cols},sum(__m) AS __n FROM {child} GROUP BY {identity} HAVING sum(__m)!=0)");
    let current = format!("(SELECT coalesce(sum(a.__n),0) FROM {t} a WHERE a.rowid=o.__v)");
    SplitStatements {
        clear_arrived: format!("DELETE FROM {arrived}"),
        clear_left: format!("DELETE FROM {left}"),
        fill_left: format!("INSERT INTO {left} SELECT {cols} FROM {delta} o WHERE o.__n<0 AND {current}=0"),
        fill_arrived: format!("INSERT INTO {arrived} SELECT {cols} FROM {delta} o WHERE o.__n>0 AND {current}=o.__n"),
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
            &roles(plan, db, false, name, id, 0, rule, None, Role::Table(all.clone())),
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
                    &roles(plan, db, false, name, id, 0, rule, None, Role::Range(all.clone(), 0, 0)),
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
                    &roles(plan, db, true, name, id, 0, rule, None, Role::Range(work.clone(), 0, 0)),
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
                                plan, db, false,
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
        split: split_statements(plan, name, db, id, side),
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
                                plan, db, true,
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
    let projected = (0..width).map(|i|format!("o.c{i}")).collect::<Vec<_>>().join(",");
    let check = format!("sqlite_ivm_row_check({projected})");
    let exact = crate::native_keys::exact_row_columns(width,"o.").join(",");
    let matched = crate::native_keys::exact_row_match(width,"s.","d.");
    let previous = crate::native_keys::exact_row_match(width,"p.","s.");
    let mut statements = ApplyStateStatements {
        wanted: format!("SELECT coalesce(sum(-__m),0) FROM {out} o WHERE __m<0"),
        replace: None,
        retract: format!(
            "DELETE FROM {state} WHERE rowid IN (SELECT s.rowid FROM {state} s JOIN (SELECT {projected},sum(-__m) AS __n FROM {out} o WHERE __m<0 GROUP BY {exact}) d ON {matched} \
             WHERE (SELECT count(*) FROM {state} p WHERE {previous} AND p.rowid<=s.rowid)<=d.__n)"
        ),
        peak: format!("SELECT coalesce(max(__m),0) FROM {out}"),
        extend: format!(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce(max(__m),0) FROM {out})) \
             INSERT INTO {state}(__check,{}) SELECT {check},{projected} FROM {out} o,seq WHERE o.__m>0 AND seq.n<=o.__m",
            columns(width)
        ),
    };
    if let Some(keys) = plan.stored_group_key().filter(|_| matches!(node.kind,Kind::Group { window:false,limit:None,.. })) {
        let positions = if keys.is_empty() { vec![] } else { keys.split(',').collect::<Vec<_>>() };
        let same_group = |left: &str, right: &str| {
            if positions.is_empty() { "1".into() } else {
                positions.iter().map(|c|format!("{left}.{c} IS (+{right}.{c})")).collect::<Vec<_>>().join(" AND ")
            }
        };
        let positive = same_group("s","o");
        let negative = same_group("s","d");
        let output_for_state = same_group("o","s");
        let output_for_delta = same_group("o","d");
        let candidates = format!("SELECT s.rowid FROM {out} d CROSS JOIN {state} s ON {negative} WHERE d.__m<0");
        statements.replace = Some(format!("UPDATE {state} AS s SET ({},__check)=(SELECT {projected},{check} FROM {out} o WHERE o.__m>0 AND {output_for_state}) WHERE s.rowid IN ({candidates} AND EXISTS(SELECT 1 FROM {out} o WHERE o.__m>0 AND {output_for_delta}))",columns(width)));
        statements.retract = format!("DELETE FROM {state} WHERE rowid IN ({candidates} AND NOT EXISTS(SELECT 1 FROM {out} o WHERE o.__m>0 AND {output_for_delta}))");
        statements.extend = format!("INSERT INTO {state}(__check,{}) SELECT {check},{projected} FROM {out} o WHERE o.__m>0 AND NOT EXISTS(SELECT 1 FROM {state} s WHERE {positive})",columns(width));
    }
    statements
}
