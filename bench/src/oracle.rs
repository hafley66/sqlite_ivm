//! Independent fixture oracle: JS-semantics port of `bench/shared/30_circuit_workload.mjs`
//! (`circuits`, `valueDomains`, `sortRows`, `outputText`, `inputText`, `circuitOracle`)
//! and `bench/shared/36_semantic_catalog.mjs` (`semanticOracle`). No SQL executes here.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Cell {
    Int(i64),
    Text(String),
}

impl Cell {
    pub fn text(s: impl Into<String>) -> Cell {
        Cell::Text(s.into())
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Cell::Int(n) => Some(*n),
            Cell::Text(_) => None,
        }
    }
}

/// `compareCell` from 30_circuit_workload.mjs: numeric only when both cells are
/// numbers, otherwise JS `String(a) < String(b)` (code-unit order; ASCII here).
pub fn compare_cell(a: &Cell, b: &Cell) -> Ordering {
    match (a, b) {
        (Cell::Int(x), Cell::Int(y)) => x.cmp(y),
        _ => cell_string(a).cmp(&cell_string(b)),
    }
}

pub fn cell_string(c: &Cell) -> String {
    match c {
        Cell::Int(n) => n.to_string(),
        Cell::Text(s) => s.clone(),
    }
}

pub fn sort_rows(mut rows: Vec<Vec<Cell>>) -> Vec<Vec<Cell>> {
    rows.sort_by(|x, y| {
        for i in 0..x.len().min(y.len()) {
            let order = compare_cell(&x[i], &y[i]);
            if order != Ordering::Equal {
                return order;
            }
        }
        Ordering::Equal
    });
    rows
}

/// `outputText`: `S\t` rows over already-sorted rows.
pub fn output_text(rows: &[Vec<Cell>]) -> String {
    let mut text = String::new();
    for row in rows {
        text.push_str("S\t");
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            text.push_str(&cell_string(cell));
        }
        text.push('\n');
    }
    text
}

/// `inputText`: `A/B/C<TAB>id<TAB>k<TAB>v` lines over the sorted tables.
pub fn input_text(t: &Tables) -> String {
    let mut text = String::new();
    for (name, rows) in [("A", &t.a), ("B", &t.b), ("C", &t.c)] {
        for row in rows {
            let cells: Vec<String> = row.iter().map(cell_string).collect();
            text.push_str(&format!("{name}\t{}\n", cells.join("\t")));
        }
    }
    text
}

pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Integers,
    TextNocase,
    MixedIntReal,
}

impl Domain {
    pub fn parse(name: &str) -> Option<Domain> {
        match name {
            "integers" => Some(Domain::Integers),
            "text_nocase" => Some(Domain::TextNocase),
            "mixed_int_real" => Some(Domain::MixedIntReal),
            _ => None,
        }
    }
}

/// `domainKey`: normalization used for join/distinct keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Num(i64),
    Str(String),
}

pub fn domain_key(cell: &Cell, domain: Domain) -> Key {
    match domain {
        Domain::TextNocase => Key::Str(cell_string(cell).to_lowercase()),
        Domain::MixedIntReal => Key::Num(match cell {
            Cell::Int(n) => *n,
            Cell::Text(s) => s.parse().expect("mixed_int_real cells are integral"),
        }),
        Domain::Integers => match cell {
            Cell::Int(n) => Key::Num(*n),
            Cell::Text(s) => Key::Str(s.clone()),
        },
    }
}

fn same_key(left: &Cell, right: &Cell, domain: Domain) -> bool {
    domain_key(left, domain) == domain_key(right, domain)
}

pub struct Tables {
    pub a: Vec<Vec<Cell>>,
    pub b: Vec<Vec<Cell>>,
    pub c: Vec<Vec<Cell>>,
}

/// `circuitOracle`. Rows are `[id,k,v]`; outputs are projected cells.
pub fn circuit_oracle(family: &str, t: &Tables, domain: Domain) -> anyhow::Result<Vec<Vec<Cell>>> {
    let project = |rows: &[Vec<Cell>]| -> Vec<Vec<Cell>> {
        rows.iter().map(|r| vec![r[1].clone(), r[2].clone()]).collect()
    };
    let output = match family {
        "pipeline" => t
            .a
            .iter()
            .filter(|r| r[2].as_int().unwrap() >= 0)
            .map(|r| vec![r[1].clone(), Cell::Int(r[2].as_int().unwrap() * 2)])
            .collect(),
        "fanout_fanin" => project(
            &t.a.iter()
                .filter(|r| r[2].as_int().unwrap() >= 0)
                .chain(t.a.iter().filter(|r| r[2].as_int().unwrap() % 2 == 0))
                .cloned()
                .collect::<Vec<_>>(),
        ),
        "distinct" => {
            let mut seen = std::collections::HashSet::new();
            let mut rows = Vec::new();
            for r in &t.a {
                let key = (domain_key(&r[1], domain), r[2].clone());
                if seen.insert(key) {
                    rows.push(vec![r[1].clone(), r[2].clone()]);
                }
            }
            rows
        }
        "join" => {
            let mut rows = Vec::new();
            for x in &t.a {
                for y in &t.b {
                    if same_key(&x[1], &y[1], domain) {
                        rows.push(vec![
                            x[1].clone(),
                            Cell::Int(x[2].as_int().unwrap() * y[2].as_int().unwrap()),
                        ]);
                    }
                }
            }
            rows
        }
        "self_join" => {
            let mut rows = Vec::new();
            for x in &t.a {
                for y in &t.a {
                    if same_key(&x[2], &y[1], domain) {
                        rows.push(vec![x[1].clone(), y[2].clone()]);
                    }
                }
            }
            rows
        }
        "chain" => {
            let mut rows = Vec::new();
            for x in &t.a {
                for y in &t.b {
                    if !same_key(&x[2], &y[1], domain) {
                        continue;
                    }
                    for z in &t.c {
                        if same_key(&y[2], &z[1], domain) {
                            rows.push(vec![x[1].clone(), z[2].clone()]);
                        }
                    }
                }
            }
            rows
        }
        "diamond" => {
            let mut rows = Vec::new();
            for x in &t.a {
                for y in t.b.iter().chain(t.c.iter()) {
                    if same_key(&x[2], &y[1], domain) {
                        rows.push(vec![x[1].clone(), y[2].clone()]);
                    }
                }
            }
            rows
        }
        "semijoin" => project(
            &t.a.iter()
                .filter(|x| t.b.iter().any(|y| same_key(&x[1], &y[1], domain)))
                .cloned()
                .collect::<Vec<_>>(),
        ),
        "antijoin" => project(
            &t.a.iter()
                .filter(|x| !t.b.iter().any(|y| same_key(&x[1], &y[1], domain)))
                .cloned()
                .collect::<Vec<_>>(),
        ),
        "aggregate_churn" => {
            let joined = circuit_oracle("join", t, domain)?;
            let mut order: Vec<Key> = Vec::new();
            let mut groups: HashMap<Key, (Cell, i64, i64)> = HashMap::new();
            for row in &joined {
                let normalized = domain_key(&row[0], domain);
                let entry = groups.entry(normalized.clone()).or_insert_with(|| {
                    order.push(normalized.clone());
                    (row[0].clone(), 0, 0)
                });
                entry.1 += 1;
                entry.2 += row[1].as_int().unwrap();
            }
            order
                .into_iter()
                .map(|k| {
                    let (key, n, s) = groups.remove(&k).unwrap();
                    vec![key, Cell::Int(n), Cell::Int(s)]
                })
                .collect()
        }
        "reach_cycle" => {
            let mut reached: Vec<Cell> = Vec::new();
            let add = |value: &Cell, reached: &mut Vec<Cell>| -> bool {
                if reached.iter().any(|e| same_key(e, value, domain)) {
                    return false;
                }
                reached.push(value.clone());
                true
            };
            for row in &t.b {
                add(&row[1], &mut reached);
            }
            let mut remaining = t.a.len() + 1;
            loop {
                let mut changed = false;
                for row in &t.a {
                    if reached.iter().any(|e| same_key(e, &row[1], domain))
                        && add(&row[2], &mut reached)
                    {
                        changed = true;
                    }
                }
                if !changed {
                    return Ok(reached.into_iter().map(|k| vec![k]).collect());
                }
                remaining -= 1;
                if remaining == 0 {
                    anyhow::bail!("reach_cycle iteration budget exhausted");
                }
            }
        }
        other => anyhow::bail!("unknown circuit {other}"),
    };
    Ok(output)
}

/// `semanticOracle` over tables `{a,b}`.
pub fn semantic_oracle(family: &str, t: &Tables) -> anyhow::Result<Vec<Vec<Cell>>> {
    let project = |rows: &[Vec<Cell>]| -> Vec<Vec<Cell>> {
        rows.iter().map(|r| vec![r[1].clone(), r[2].clone()]).collect()
    };
    let unique = |rows: Vec<Vec<Cell>>| -> Vec<Vec<Cell>> {
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|r| seen.insert(r.clone()))
            .collect()
    };
    let mut groups: Vec<(Cell, Vec<&Vec<Cell>>)> = Vec::new();
    for row in &t.a {
        match groups.iter_mut().find(|(k, _)| *k == row[1]) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((row[1].clone(), vec![row])),
        }
    }
    let output = match family {
        "minmax" => groups
            .iter()
            .map(|(k, rows)| {
                let vs = rows.iter().map(|r| r[2].as_int().unwrap());
                vec![
                    k.clone(),
                    Cell::Int(rows.len() as i64),
                    Cell::Int(vs.clone().min().unwrap()),
                    Cell::Int(vs.max().unwrap()),
                ]
            })
            .collect(),
        "count_distinct" => groups
            .iter()
            .map(|(k, rows)| {
                let distinct: std::collections::HashSet<&Cell> =
                    rows.iter().map(|r| &r[2]).collect();
                vec![k.clone(), Cell::Int(distinct.len() as i64)]
            })
            .collect(),
        "union_set" => unique(project(&t.a.iter().chain(t.b.iter()).cloned().collect::<Vec<_>>())),
        "except_set" => unique(project(&t.a))
            .into_iter()
            .filter(|kv| {
                !t.b.iter()
                    .any(|r| r[1] == kv[0] && r[2] == kv[1])
            })
            .collect(),
        "intersect_set" => unique(project(&t.a))
            .into_iter()
            .filter(|kv| t.b.iter().any(|r| r[1] == kv[0] && r[2] == kv[1]))
            .collect(),
        "topk" => {
            let mut sorted = t.a.clone();
            sorted.sort_by(|x, y| {
                y[2].as_int().unwrap().cmp(&x[2].as_int().unwrap())
                    .then(x[0].as_int().unwrap().cmp(&y[0].as_int().unwrap()))
            });
            project(&sorted[..3.min(sorted.len())])
        }
        "window_rank" => {
            let mut rows = Vec::new();
            for (k, group) in &groups {
                let mut sorted = group.clone();
                sorted.sort_by(|x, y| {
                    x[2].as_int().unwrap().cmp(&y[2].as_int().unwrap())
                        .then(x[0].as_int().unwrap().cmp(&y[0].as_int().unwrap()))
                });
                for (i, r) in sorted.iter().enumerate() {
                    rows.push(vec![k.clone(), r[2].clone(), Cell::Int(i as i64 + 1)]);
                }
            }
            rows
        }
        "subquery" | "cte" => project(
            &t.a.iter()
                .filter(|r| r[2].as_int().unwrap() >= 0)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        other => anyhow::bail!("unknown semantic circuit {other}"),
    };
    Ok(output)
}
