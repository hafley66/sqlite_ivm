//! Fixture generation in Rust, byte-compatible with the script-era node
//! builders `makeCircuitFixture` / `makeSemanticFixture` (sources live in git
//! history, before the script-era removal), including JSON key order and JS
//! number/string semantics. Fixtures are generated, never read from JSON files.

use crate::oracle::{
    cell_string, circuit_oracle, digest, input_text, semantic_oracle, sort_rows, Cell, Domain,
    Tables,
};
use anyhow::bail;
use serde::Serialize;

pub const CIRCUITS: [&str; 11] = [
    "pipeline",
    "fanout_fanin",
    "distinct",
    "join",
    "self_join",
    "chain",
    "diamond",
    "semijoin",
    "antijoin",
    "aggregate_churn",
    "reach_cycle",
];

pub const SEMANTIC: [&str; 9] = [
    "minmax",
    "count_distinct",
    "union_set",
    "except_set",
    "intersect_set",
    "topk",
    "window_rank",
    "subquery",
    "cte",
];

pub fn circuit_names() -> Vec<&'static str> {
    CIRCUITS.into_iter().chain(SEMANTIC).collect()
}

pub fn circuit_query(name: &str) -> Option<&'static str> {
    Some(match name {
        "pipeline" => "SELECT k AS c0,v*2 AS c1 FROM a WHERE v>=0",
        "fanout_fanin" => "SELECT k AS c0,v AS c1 FROM a WHERE v>=0 UNION ALL SELECT k AS c0,v AS c1 FROM a WHERE v%2=0",
        "distinct" => "SELECT DISTINCT k AS c0,v AS c1 FROM a",
        "join" => "SELECT a.k AS c0,a.v*b.v AS c1 FROM a JOIN b ON a.k=b.k",
        "self_join" => "SELECT x.k AS c0,y.v AS c1 FROM a x JOIN a y ON x.v=y.k",
        "chain" => "SELECT a.k AS c0,c.v AS c1 FROM a JOIN b ON a.v=b.k JOIN c ON b.v=c.k",
        "diamond" => "SELECT a.k AS c0,b.v AS c1 FROM a JOIN b ON a.v=b.k UNION ALL SELECT a.k AS c0,c.v AS c1 FROM a JOIN c ON a.v=c.k",
        "semijoin" => "SELECT a.k AS c0,a.v AS c1 FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.k=a.k)",
        "antijoin" => "SELECT a.k AS c0,a.v AS c1 FROM a WHERE NOT EXISTS (SELECT 1 FROM b WHERE b.k=a.k)",
        "aggregate_churn" => "SELECT a.k AS c0,COUNT(*) AS c1,SUM(a.v*b.v) AS c2 FROM a JOIN b ON a.k=b.k GROUP BY a.k",
        "reach_cycle" => "WITH RECURSIVE reachable(node) AS (SELECT k FROM b UNION SELECT a.v FROM a JOIN reachable r ON a.k=r.node) SELECT node AS c0 FROM reachable",
        _ => return None,
    })
}

pub fn semantic_spec(name: &str) -> Option<(&'static str, usize)> {
    Some(match name {
        "minmax" => ("SELECT k AS c0,COUNT(*) AS c1,MIN(v) AS c2,MAX(v) AS c3 FROM a GROUP BY k", 4),
        "count_distinct" => ("SELECT k AS c0,COUNT(DISTINCT v) AS c1 FROM a GROUP BY k", 2),
        "union_set" => ("SELECT k AS c0,v AS c1 FROM a UNION SELECT k AS c0,v AS c1 FROM b", 2),
        "except_set" => ("SELECT k AS c0,v AS c1 FROM a EXCEPT SELECT k AS c0,v AS c1 FROM b", 2),
        "intersect_set" => ("SELECT k AS c0,v AS c1 FROM a INTERSECT SELECT k AS c0,v AS c1 FROM b", 2),
        "topk" => ("SELECT k AS c0,v AS c1 FROM a ORDER BY v DESC,id LIMIT 3", 2),
        "window_rank" => ("SELECT k AS c0,v AS c1,ROW_NUMBER() OVER (PARTITION BY k ORDER BY v,id) AS c2 FROM a", 3),
        "subquery" => ("SELECT k AS c0,v AS c1 FROM (SELECT k,v FROM a WHERE v>=0) q", 2),
        "cte" => ("WITH q AS (SELECT k,v FROM a WHERE v>=0) SELECT k AS c0,v AS c1 FROM q", 2),
        _ => return None,
    })
}

/// Output column count for pg materialization: explicit for semantic circuits,
/// otherwise the runner derivation (aggregate 3, reach 1, else 2).
pub fn circuit_column_count(name: &str) -> usize {
    match name {
        "aggregate_churn" => 3,
        "reach_cycle" => 1,
        _ => 2,
    }
}

#[derive(Serialize, Clone)]
pub struct Fixture {
    pub circuit: String,
    pub query: String,
    pub rows: i64,
    pub batch_size: i64,
    pub fanout: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_domain: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_schema: Option<TableSchema>,
    pub states: Vec<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<usize>,
}

impl Fixture {
    pub fn column_count(&self) -> usize {
        self.columns
            .unwrap_or_else(|| circuit_column_count(&self.circuit))
    }
}

#[derive(Serialize, Clone)]
pub struct TableSchema {
    pub k_sql: &'static str,
    pub v_sql: &'static str,
}

#[derive(Serialize, Clone)]
pub struct State {
    pub name: String,
    pub writes: Vec<Write>,
    pub mutation_sql: String,
    pub inputs: Inputs,
    pub input_hash: String,
    pub expected: Expected,
}

#[derive(Serialize, Clone)]
pub struct Write {
    pub table: &'static str,
    pub id: i64,
    pub row: Option<WriteRow>,
}

/// `[id,k,v]` triple, serialized as a JSON array.
#[derive(Serialize, Clone, PartialEq, Eq)]
pub struct WriteRow(pub i64, pub Cell, pub Cell);

#[derive(Serialize, Clone, Default)]
pub struct Inputs {
    pub a: Vec<WriteRow>,
    pub b: Vec<WriteRow>,
    pub c: Vec<WriteRow>,
}

impl Inputs {
    pub fn table_mut(&mut self, name: &str) -> &mut Vec<WriteRow> {
        match name {
            "a" => &mut self.a,
            "b" => &mut self.b,
            _ => &mut self.c,
        }
    }
    pub fn sorted(&self) -> Tables {
        let sorted = |rows: &[WriteRow]| -> Vec<Vec<Cell>> {
            sort_rows(
                rows.iter()
                    .map(|r| vec![Cell::Int(r.0), r.1.clone(), r.2.clone()])
                    .collect(),
            )
        };
        Tables { a: sorted(&self.a), b: sorted(&self.b), c: sorted(&self.c) }
    }
}

#[derive(Serialize, Clone)]
pub struct Expected {
    pub rows: Vec<Vec<Cell>>,
    pub checksum: String,
}

/// `valueRow`.
fn value_row(
    domain: Domain,
    id: i64,
    k: Cell,
    v: Cell,
    real: bool,
    case_variant: bool,
) -> WriteRow {
    match domain {
        Domain::TextNocase => {
            let prefix = if case_variant && id % 2 == 0 { "N" } else { "n" };
            let key = cell_string(&k).replace('-', "_");
            WriteRow(id, Cell::text(format!("{prefix}{key}")), v)
        }
        Domain::MixedIntReal => {
            let key = if real { Cell::text(format!("{}.0", cell_string(&k))) } else { k };
            WriteRow(id, key, v)
        }
        Domain::Integers => WriteRow(id, k, v),
    }
}

/// `sqlCell`.
fn sql_cell(value: &Cell, domain: Domain) -> String {
    match value {
        Cell::Int(n) => n.to_string(),
        Cell::Text(s) => {
            let raw = domain == Domain::MixedIntReal && is_int_or_zero_decimal(s);
            if raw {
                s.clone()
            } else {
                format!("'{}'", s.replace('\'', "''"))
            }
        }
    }
}

fn is_int_or_zero_decimal(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    match body.split_once('.') {
        None => !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()),
        Some((int, frac)) => {
            !int.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac == "0"
        }
    }
}

struct Builder<'a> {
    family: &'a str,
    domain: Domain,
    tables: Inputs,
    states: Vec<State>,
}

impl<'a> Builder<'a> {
    fn seal(&mut self, name: &str, writes: Vec<Write>) -> anyhow::Result<()> {
        let mut sql = Vec::new();
        for write in &writes {
            let table = self.tables.table_mut(write.table);
            table.retain(|r| r.0 != write.id);
            sql.push(format!("DELETE FROM {} WHERE id={};", write.table, write.id));
            if let Some(row) = &write.row {
                if row.0.abs() > 1_000_000 || matches!(&row.2, Cell::Int(v) if v.abs() > 1_000_000)
                {
                    bail!("fixture bounds");
                }
                table.push(row.clone());
                sql.push(format!(
                    "INSERT INTO {}(id,k,v) VALUES({},{},{});",
                    write.table,
                    sql_cell(&Cell::Int(row.0), self.domain),
                    sql_cell(&row.1, self.domain),
                    sql_cell(&row.2, self.domain),
                ));
            }
        }
        let inputs = self.tables.clone();
        let sorted = inputs.sorted();
        let output = sort_rows(circuit_oracle(self.family, &sorted, self.domain)?);
        self.states.push(State {
            name: name.to_string(),
            writes,
            mutation_sql: sql.join("\n"),
            input_hash: digest(&input_text(&sorted)),
            inputs,
            expected: Expected {
                checksum: digest(&crate::oracle::output_text(&output)),
                rows: output,
            },
        });
        Ok(())
    }
}

/// `makeCircuitFixture`.
pub fn make_circuit_fixture(
    family: &str,
    rows: i64,
    batch: i64,
    fanout: i64,
    domain: Domain,
) -> anyhow::Result<Fixture> {
    if circuit_query(family).is_none() {
        bail!("unknown circuit {family}");
    }
    if !(1..=12000).contains(&rows) || batch < 1 || batch > rows || fanout < 1 {
        bail!("bounded fixture dimensions required");
    }
    let mut b = Builder { family, domain, tables: Inputs::default(), states: Vec::new() };
    let mut initial = Vec::new();
    for id in 1..=rows {
        initial.push(Write {
            table: "a",
            id,
            row: Some(value_row(
                domain,
                id,
                Cell::Int((id - 1) / fanout),
                Cell::Int(id % 7 - 3),
                false,
                false,
            )),
        });
    }
    for table in ["b", "c"] {
        for id in 1..=7 {
            let v = if table == "b" { id % 3 - 1 } else { id % 4 };
            initial.push(Write {
                table,
                id,
                row: Some(value_row(domain, id, Cell::Int(id - 4), Cell::Int(v), true, false)),
            });
        }
    }
    b.seal("initial", initial)?;
    let first_key = b.tables.a[0].1.clone();
    let first_value = b.tables.a[0].2.clone();
    b.seal(
        "duplicate_support",
        vec![Write {
            table: "a",
            id: rows + 1,
            // JS copies a[0]'s already-encoded cells verbatim.
            row: Some(WriteRow(rows + 1, first_key, first_value)),
        }],
    )?;
    b.seal("retract_one_support", vec![Write { table: "a", id: 1, row: None }])?;
    b.seal(
        "batch_key_value_move",
        (0..batch)
            .map(|i| Write {
                table: "a",
                id: i + 2,
                row: Some(value_row(domain, i + 2, Cell::Int(0), Cell::Int(-(i % 3)), false, false)),
            })
            .collect(),
    )?;
    b.seal(
        "right_support_move",
        vec![
            Write { table: "b", id: 1, row: Some(value_row(domain, 1, Cell::Int(0), Cell::Int(0), true, false)) },
            Write { table: "b", id: 2, row: Some(value_row(domain, 2, Cell::Int(0), Cell::Int(-2), true, false)) },
        ],
    )?;
    b.seal(
        "third_side_move",
        vec![Write { table: "c", id: 1, row: Some(value_row(domain, 1, Cell::Int(0), Cell::Int(-3), true, false)) }],
    )?;
    b.seal(
        "clear_roots",
        b.tables.b.iter().map(|r| Write { table: "b", id: r.0, row: None }).collect(),
    )?;
    b.seal(
        "clear_edges",
        b.tables.a.iter().map(|r| Write { table: "a", id: r.0, row: None }).collect(),
    )?;
    let cycle_variants = family == "reach_cycle";
    let mut cycle_seed = vec![Write {
        table: "b",
        id: 1,
        row: Some(value_row(domain, 1, Cell::Int(1), Cell::Int(1), true, cycle_variants)),
    }];
    for (id, k, v) in [(1, 1, 2), (2, 2, 3), (3, 3, 2), (4, 1, 4), (5, 4, 3)] {
        cycle_seed.push(Write {
            table: "a",
            id,
            row: Some(value_row(domain, id, Cell::Int(k), Cell::Int(v), false, cycle_variants)),
        });
    }
    b.seal("cycle_seed", cycle_seed)?;
    b.seal("diamond_path_retract", vec![Write { table: "a", id: 2, row: None }])?;
    b.seal("root_retract", vec![Write { table: "b", id: 1, row: None }])?;
    b.seal(
        "root_restore",
        vec![Write { table: "b", id: 1, row: Some(value_row(domain, 1, Cell::Int(1), Cell::Int(1), true, cycle_variants)) }],
    )?;
    b.seal("cycle_break", vec![Write { table: "a", id: 3, row: None }])?;
    Ok(Fixture {
        circuit: family.to_string(),
        query: circuit_query(family).unwrap().to_string(),
        rows,
        batch_size: batch,
        fanout,
        value_domain: (domain != Domain::Integers).then_some(match domain {
            Domain::TextNocase => "text_nocase",
            _ => "mixed_int_real",
        }),
        table_schema: (domain != Domain::Integers).then_some(TableSchema {
            k_sql: match domain {
                Domain::TextNocase => "TEXT NOT NULL COLLATE NOCASE",
                _ => "NOT NULL",
            },
            v_sql: "INTEGER NOT NULL",
        }),
        states: b.states,
        columns: None,
    })
}

/// `makeSemanticFixture`.
pub fn make_semantic_fixture(
    family: &str,
    rows: i64,
    batch: i64,
    fanout: i64,
    domain: Domain,
) -> anyhow::Result<Fixture> {
    let Some((query, columns)) = semantic_spec(family) else {
        bail!("unknown semantic circuit {family}");
    };
    let effective = if domain == Domain::MixedIntReal { Domain::Integers } else { domain };
    let mut fixture = make_circuit_fixture("pipeline", rows, batch, fanout, effective)?;
    fixture.circuit = family.to_string();
    fixture.query = query.to_string();
    fixture.columns = Some(columns);
    for state in &mut fixture.states {
        let tables = state.inputs.sorted();
        let result = sort_rows(semantic_oracle(family, &tables)?);
        state.expected = Expected { checksum: digest(&crate::oracle::output_text(&result)), rows: result };
    }
    Ok(fixture)
}

pub fn make_fixture(
    family: &str,
    rows: i64,
    batch: i64,
    fanout: i64,
    domain: Domain,
) -> anyhow::Result<Fixture> {
    if SEMANTIC.contains(&family) {
        make_semantic_fixture(family, rows, batch, fanout, domain)
    } else {
        make_circuit_fixture(family, rows, batch, fanout, domain)
    }
}
