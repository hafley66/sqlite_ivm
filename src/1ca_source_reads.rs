//! Operator inputs are SQL reads of authoritative sources and derived results.
//! A before-state read subtracts the pending input delta from the current bag.
use crate::{
    catalog::quote,
    columns::{column_references, substitute_columns},
    relational::{Kind, Plan},
    relational_maintenance::{columns, identity_sql, keys_table, out_table, table},
    relational_materialize::MaterializeStatements,
};
use rusqlite::Connection;

impl Plan {
    pub(crate) fn source_expression(&self, id: usize, expression: &str) -> Option<(usize, String)> {
        if column_references(expression).is_empty() {
            return None;
        }
        let node = &self.nodes[id];
        match &node.kind {
            Kind::Input(source) => Some((
                *source,
                substitute_columns(expression, |i| quote(&self.sources[*source].columns[i])),
            )),
            Kind::Map { expressions, .. } => self.source_expression(
                node.inputs[0],
                &substitute_columns(expression, |i| format!("({})", expressions[i])),
            ),
            Kind::Join { mode: "inner", .. } => {
                let width = self.nodes[node.inputs[0]].fields.len();
                let refs = column_references(expression);
                if refs.is_empty() {
                    return None;
                }
                if refs.iter().all(|i| *i < width) {
                    self.source_expression(node.inputs[0], expression)
                } else if refs.iter().all(|i| *i >= width) {
                    self.source_expression(
                        node.inputs[1],
                        &crate::columns::renumber_columns(expression, |i| i - width),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Full current bag, generated from the same materialization expressions.
    /// Recursive members are derived results, never copies of operator inputs.
    pub(crate) fn live_rows(&self, db: &Connection, name: &str, id: usize) -> String {
        fn visit(
            plan: &Plan,
            id: usize,
            seen: &mut std::collections::BTreeSet<usize>,
            order: &mut Vec<usize>,
        ) {
            if !seen.insert(id) {
                return;
            }
            if !matches!(plan.nodes[id].kind, Kind::Fixpoint { .. }) {
                for input in &plan.nodes[id].inputs {
                    visit(plan, *input, seen, order);
                }
            }
            order.push(id);
        }
        let mut order = Vec::new();
        visit(self, id, &mut std::collections::BTreeSet::new(), &mut order);
        let definitions = order
            .into_iter()
            .map(|input| {
                format!(
                    "__ivm_read_{input} AS ({})",
                    self.live_rows_definition(db, name, input)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("WITH {definitions} SELECT * FROM __ivm_read_{id}")
    }

    // References keep shared relational subgraphs from expanding into repeated
    // SQL text. SQLite chooses each CTE's execution strategy; no persistent
    // operator-input tables are introduced.
    fn live_rows_definition(&self, db: &Connection, name: &str, id: usize) -> String {
        let node = &self.nodes[id];
        let width = node.fields.len();
        let cols = columns(width);
        match &node.kind {
            Kind::Input(source) => format!(
                "SELECT {},1 AS __n FROM main.{}",
                self.sources[*source]
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{} AS c{i}", quote(c)))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&self.sources[*source].name)
            ),
            Kind::Map {
                expressions,
                predicate,
            } => format!(
                "SELECT {},__n FROM ({}){}",
                expressions
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!("{e} AS c{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                format!("SELECT * FROM __ivm_read_{}", node.inputs[0]),
                predicate
                    .as_ref()
                    .map(|p| format!(" WHERE {p}"))
                    .unwrap_or_default()
            ),
            Kind::Set("all") => node
                .inputs
                .iter()
                .map(|input| format!("SELECT * FROM __ivm_read_{input}"))
                .collect::<Vec<_>>()
                .join(" UNION ALL "),
            Kind::Fixpoint { .. } => format!(
                "SELECT {cols},1 AS __n FROM {}",
                table(name, id, node.inputs.len())
            ),
            _ => {
                let sources = (0..node.inputs.len())
                    .map(|side| {
                        self.live_input_from_rows(
                            name,
                            id,
                            side,
                            false,
                            false,
                            format!("SELECT * FROM __ivm_read_{}", node.inputs[side]),
                        )
                    })
                    .collect::<Vec<_>>();
                let statement = self.materialize_from_sources(db, name, id, false, Some(&sources));
                let insert = match statement {
                    MaterializeStatements::Set { insert }
                    | MaterializeStatements::Join { insert, .. } => insert,
                    MaterializeStatements::Group(g) if g.window => g.window_insert,
                    MaterializeStatements::Group(g) if g.limit.is_some() => {
                        g.limit_insert.replace("__k=?1", "1")
                    }
                    MaterializeStatements::Group(g) => g.plain_insert,
                    _ => unreachable!(),
                };
                let target = format!("INSERT INTO {}({cols},__m) ", out_table(id, width));
                // Remove this compiler's exact destination, preserving WITH
                // clauses and all scalar/aggregate expressions verbatim.
                let query = insert.replacen(&target, "", 1);
                let aliases = (0..width)
                    .map(|i| format!("c{i}"))
                    .chain(std::iter::once("__n".into()))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("WITH __ivm_live({aliases}) AS ({query}) SELECT * FROM __ivm_live")
            }
        }
    }

    pub(crate) fn live_input(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        side: usize,
        restricted: bool,
        before: bool,
    ) -> String {
        let input = self.nodes[id].inputs[side];
        self.live_input_from_rows(
            name,
            id,
            side,
            restricted,
            before,
            self.live_rows(db, name, input),
        )
    }

    fn live_input_from_rows(
        &self,
        name: &str,
        id: usize,
        side: usize,
        restricted: bool,
        before: bool,
        source_rows: String,
    ) -> String {
        let input = self.nodes[id].inputs[side];
        let width = self.nodes[input].fields.len();
        let cols = columns(width);
        let native = self.native_key_values(id, side);
        let key = if native.is_none() {
            self.key_sql(id, side).expect("membership key")
        } else {
            String::new()
        };
        let identity = identity_sql(width);
        let filter = if restricted && native.is_none() {
            format!(" WHERE {key} IN (SELECT __v FROM {} WHERE __i IN (SELECT __k FROM temp.__ivm_touched))",keys_table(name))
        } else {
            String::new()
        };
        let read = |source: String, weight: &str| {
            if restricted {
                if let Some(keys) = &native {
                    return format!(
                        "SELECT {},s.{weight} FROM {}",
                        (0..width)
                            .map(|i| format!("s.c{i}"))
                            .collect::<Vec<_>>()
                            .join(","),
                        self.touched_source(name, id, keys, &source)
                    );
                }
            }
            format!("SELECT {cols},{weight} FROM {source}{filter}")
        };
        let current = read(format!("({source_rows})"), "__n");
        let rows = if before {
            let delta = read(out_table(input, width), "__m");
            let exact = crate::native_keys::exact_row_columns(width, "").join(",");
            format!("SELECT {cols},sum(__n) AS __n FROM ({current} UNION ALL SELECT {cols},-__m AS __n FROM ({delta})) GROUP BY {exact} HAVING sum(__n)>0")
        } else if matches!(self.nodes[id].kind, Kind::Set(_)) {
            format!("SELECT {cols},sum(__n) AS __n FROM ({current}) GROUP BY {identity}")
        } else {
            current
        };
        if native.is_some() {
            let key = if matches!(self.nodes[id].kind, Kind::Join { .. }) {
                "0".into()
            } else {
                self.key_lookup(name, id, side)
            };
            format!("(SELECT {key} AS __k,{cols},__n FROM ({rows}))")
        } else {
            format!("(SELECT {identity} AS rowid,{key} AS __k,{cols},__n FROM ({rows}))")
        }
    }
}
