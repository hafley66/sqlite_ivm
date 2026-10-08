//! Cell-level helpers shared by every operator: column picks, the cross-type order, SQLite's
//! SUM over `Any` cells, and the source guard. Same semantics as `ivm-dd`.

use ivm_ir::*;
use ivm_engine::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(crate) fn cols(row: &[Cell], cols: &[ColId]) -> Row {
    cols.iter().map(|c| row[*c as usize]).collect()
}

/// The row a join emits for `a` (left, `left` columns) and `b`: both concatenated, then `project`.
pub(crate) fn join_row(a: &Row, b: &Row, left: usize, project: &Option<Vec<ColId>>) -> Row {
    match project {
        None => {
            let mut row = Vec::with_capacity(a.len() + b.len());
            row.extend_from_slice(a);
            row.extend_from_slice(b);
            row
        }
        Some(project) => project.iter().map(|c| {
            let c = *c as usize;
            if c < left { a[c] } else { b[c - left] }
        }).collect(),
    }
}

/// Relations a LetRec's bodies read through `Get`.
pub(crate) fn rec_inputs(p: &Program, rec: &LetRec) -> BTreeSet<RelId> {
    let mut inputs = BTreeSet::new();
    let mut visited = vec![false; p.nodes.len()];
    let mut pending = rec.bodies.clone();
    while let Some(id) = pending.pop() {
        let index = id as usize;
        let Some(seen) = visited.get_mut(index) else { continue; };
        if *seen { continue; }
        *seen = true;
        match &p.nodes[index] {
            Op::Get(rel) => { inputs.insert(*rel); }
            Op::Mint { input, .. } | Op::StrCons { input, .. } | Op::Str { input, .. } | Op::Mfp { input, .. }
            | Op::Reduce { input, .. } | Op::TopK { input, .. } | Op::Window { input, .. } => pending.push(*input),
            Op::Negate(input) | Op::Threshold(input) | Op::Delay(input) => pending.push(*input),
            Op::Union(nodes) | Op::Join { inputs: nodes, .. } => pending.extend(nodes.iter().copied()),
            Op::Antijoin { l, r, .. } => pending.extend([*l, *r]),
        }
    }
    inputs
}

fn numeric_prefix(bytes: &[u8]) -> f64 {
    let mut end = 0;
    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) { end += 1; }
    if end < bytes.len() && matches!(bytes[end], b'+' | b'-') { end += 1; }
    let mut digits = 0;
    while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; digits += 1; }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; digits += 1; }
    }
    if digits == 0 { return 0.0; }
    let exponent = end;
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        end += 1;
        if end < bytes.len() && matches!(bytes[end], b'+' | b'-') { end += 1; }
        let before = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
        if end == before { end = exponent; }
    }
    std::str::from_utf8(&bytes[..end]).ok().and_then(|text| text.trim().parse::<f64>().ok()).unwrap_or(0.0)
}

fn sum_add(sum: &mut f64, error: &mut f64, value: f64) {
    let old = *sum;
    let next = old + value;
    if old.abs() > value.abs() { *error += (old - next) + value; }
    else { *error += (value - next) + old; }
    *sum = next;
}
fn sum_add_integer(sum: &mut f64, error: &mut f64, value: i64) {
    if !(-4503599627370495..=4503599627370495).contains(&value) {
        let small = value % 16384;
        sum_add(sum, error, (value - small) as f64);
        sum_add(sum, error, small as f64);
    } else { sum_add(sum, error, value as f64); }
}
fn sum_init(integer: i64) -> (f64, f64) {
    if !(-4503599627370495..=4503599627370495).contains(&integer) {
        let small = integer % 16384;
        ((integer - small) as f64, small as f64)
    } else { (integer as f64, 0.0) }
}

pub(crate) fn sqlite_sum_any(values: &[(AnyValue, W)]) -> Result<AnyValue, &'static str> {
    let (mut integer, mut real, mut error, mut approximate, mut overflow, mut seen) = (0i64, 0.0f64, 0.0f64, false, false, false);
    for (value, weight) in values {
        for _ in 0..(*weight).max(0) {
            let numeric = match value {
                AnyValue::Null => continue,
                AnyValue::Integer(value) => Ok(*value),
                AnyValue::Text(value) => value.trim().parse::<i64>().map_err(|_| numeric_prefix(value.as_bytes())),
                AnyValue::Real(bits) => Err(f64::from_bits(*bits)),
                AnyValue::Blob(value) => Err(numeric_prefix(value)),
            };
            match numeric {
                Ok(value) if !approximate => {
                    if let Some(next) = integer.checked_add(value) { integer = next; }
                    else {
                        overflow = true;
                        (real, error) = sum_init(integer);
                        approximate = true;
                        sum_add_integer(&mut real, &mut error, value);
                    }
                }
                Ok(value) => sum_add_integer(&mut real, &mut error, value),
                Err(value) => {
                    if !approximate { (real, error) = sum_init(integer); approximate = true; }
                    overflow = false;
                    sum_add(&mut real, &mut error, value);
                }
            }
            seen = true;
        }
    }
    if overflow { return Err("integer overflow"); }
    let total = if error.is_infinite() { real } else { real + error };
    Ok(if !seen || approximate && total.is_nan() { AnyValue::Null }
        else if approximate { AnyValue::Real(total.to_bits()) }
        else { AnyValue::Integer(integer) })
}

pub(crate) fn cmp_cell(ty: Ty, a: Cell, b: Cell, interner: &Interner) -> Ordering {
    match ty {
        Ty::Id => interner.compare(a, b),
        Ty::Text => interner.text(a).cmp(&interner.text(b)),
        Ty::Real => f64::from_bits(a as u64).partial_cmp(&f64::from_bits(b as u64)).unwrap_or_else(|| a.cmp(&b)),
        Ty::Any => interner.any_value(a).expect("Any cell").sqlite_cmp(interner.any_value(b).expect("Any cell")),
        Ty::Int => a.cmp(&b),
    }
}

pub(crate) fn rank(order: &[Order], types: &[Ty], interner: &Interner, a: &Row, b: &Row) -> Ordering {
    order
        .iter()
        .map(|o| {
            let i = o.col as usize;
            let ord = cmp_cell(types[i], a[i], b[i], interner);
            if o.desc { ord.reverse() } else { ord }
        })
        .find(|ord| ord.is_ne())
        .unwrap_or_else(|| a.iter().zip(b).enumerate().map(|(i, (x, y))| cmp_cell(types[i], *x, *y, interner)).find(|o| o.is_ne()).unwrap_or(Ordering::Equal))
}

/// Source relations are sets. Insert of a present row rejects the frontier; delete of an
/// absent row is a no-op, like SQL `DELETE` matching nothing, and logs a warning.
pub(crate) fn guard(
    program: &Program,
    sources: &mut BTreeMap<RelId, HashSet<Row>>,
    tick: u64,
    frontier: &Frontier,
) -> Result<Vec<SourceChange>, EngineError> {
    let mut pending: BTreeMap<(RelId, Row), W> = BTreeMap::new();
    let mut accepted = Vec::new();
    for change in &frontier.changes {
        let rel = program
            .rel(change.rel)
            .filter(|r| r.kind == RelKind::Source)
            .ok_or_else(|| EngineError::new(Stage::Settle, Some(change.rel), ErrorKind::UnknownRel(change.rel)))?;
        if change.row.len() != rel.cols.len() {
            let kind = ErrorKind::Arity { expected: rel.cols.len(), actual: change.row.len() };
            return Err(EngineError::new(Stage::Settle, Some(rel.id), kind));
        }
        if change.w != 1 && change.w != -1 {
            return Err(EngineError::new(Stage::Settle, Some(rel.id), ErrorKind::Unsupported("weight other than +1/-1")));
        }
        let key = (rel.id, change.row.clone());
        let before = *pending.get(&key).unwrap_or(&0)
            + W::from(sources.get(&rel.id).unwrap().contains(&change.row));
        if change.w > 0 && before > 0 {
            return Err(EngineError::new(Stage::Settle, Some(rel.id), ErrorKind::PresentInsert(change.row.clone())));
        }
        if change.w < 0 && before <= 0 {
            tracing::warn!(tick, relation = %rel.name, row = ?change.row, "delete of absent row ignored");
            continue;
        }
        *pending.entry(key).or_default() += change.w;
        accepted.push(change.clone());
    }
    for change in &accepted {
        let rows = sources.get_mut(&change.rel).unwrap();
        if change.w > 0 { rows.insert(change.row.clone()); }
        else { rows.remove(&change.row); }
    }
    Ok(accepted)
}
