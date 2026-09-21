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
    /// Per input side: whether that side's out table holds any row.
    pub(crate) touched: Vec<String>,
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
    pub(crate) diff: [String; 6],
    pub(crate) sides: Vec<Option<ArrangementSide>>,
    pub(crate) materialize: MaterializeStatements,
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
    pub(crate) insert_new: String,
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
        touched: node
            .inputs
            .iter()
            .map(|input| {
                let child = out_table(*input, plan.nodes[*input].fields.len());
                format!("SELECT EXISTS(SELECT 1 FROM {child})")
            })
            .collect(),
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
                KindStatements::Fixpoint(fixpoint_statements(plan, name, id, width))
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
            format!("INSERT INTO {before} SELECT {cols},sum(__m) FROM {out} GROUP BY {identity} HAVING sum(__m)!=0"),
            format!("DELETE FROM {out}"),
            format!("INSERT INTO {out} SELECT * FROM {before}"),
            format!("DELETE FROM {before}"),
        ],
        sides: (0..node.inputs.len())
            .map(|side| arrangement_side(plan, name, id, side))
            .collect(),
        materialize: plan.materialize_statements(db, name, id, true),
    }
}

fn arrangement_side(plan: &Plan, name: &str, id: usize, side: usize) -> Option<ArrangementSide> {
    let node = &plan.nodes[id];
    let child = out_table(node.inputs[side], plan.nodes[node.inputs[side]].fields.len());
    let dict = keys_table(name);
    let key = plan.key_sql(id, side)?;
    Some(ArrangementSide {
        intern: format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {key} FROM {child}"),
        touch: format!("INSERT OR IGNORE INTO temp.__ivm_touched SELECT __i FROM {dict} WHERE __v IN (SELECT {key} FROM {child})"),
        upsert: upsert_statements(plan, name, id, side, &key),
    })
}

fn upsert_statements(
    plan: &Plan,
    name: &str,
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
    UpsertStatements {
        clear_delta: format!("DELETE FROM {delta}"),
        fill_delta: format!(
            "INSERT INTO {delta}(__r,__v,__n,{cols}) SELECT sqlite_ivm_hash(__ivm_v),__ivm_v,__ivm_n,{cols} \
             FROM (SELECT {identity} AS __ivm_v,sum(__m) AS __ivm_n,{cols} FROM {child} GROUP BY {identity}) WHERE __ivm_n!=0"
        ),
        apply: format!(
            "UPDATE {t} SET __n={t}.__n+d.__n FROM {delta} d WHERE d.__r={t}.__r AND d.__v={arrangement_identity}"
        ),
        insert_new: format!(
            "INSERT INTO {t}(__k,__r,__n,{cols}) SELECT (SELECT __i FROM {dict} WHERE __v={key}),d.__r,d.__n,{cols} FROM {delta} d \
             WHERE NOT EXISTS(SELECT 1 FROM {t} a WHERE a.__r=d.__r AND {}=d.__v)",
            identity_of("a")
        ),
        bad: format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE __n<0 OR typeof(__n)!='integer')"),
        drop_zero: format!("DELETE FROM {t} WHERE __n=0"),
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

fn fixpoint_statements(plan: &Plan, name: &str, id: usize, width: usize) -> FixpointStatements {
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
            .map(|side| fixpoint_side(plan, name, id, side, &all, &work))
            .collect(),
    }
}

fn fixpoint_side(
    plan: &Plan,
    name: &str,
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
        upsert: upsert_statements(plan, name, id, side, &key),
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
    let state = format!("main.{}", crate::query::quote(&format!("{name}_state")));
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
