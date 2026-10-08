//! DBSP engine: `lower` runs once inside `Runtime::init_circuit`; each `settle` is one or more
//! transactions. A LetRec is a `recursive_dynamic` child circuit.

use crate::cells::*;
use dbsp::operator::Fold;
use dbsp::typed_batch::IndexedZSetReader;
use dbsp::{NestedCircuit, OrdIndexedZSet, OrdZSet, OutputHandle, RootCircuit, Runtime, Stream, ZSetHandle, ZWeight};
use ivm_engine::*;
use ivm_ir::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

type Z<C> = Stream<C, OrdZSet<Row>>;
type Keyed<C> = Stream<C, OrdIndexedZSet<Row, Row>>;
type Group = Vec<(Row, W)>;

/// State every scope of one install reads: the dictionary and the program's constants.
#[derive(Clone)]
struct Shared {
    interner: Arc<Mutex<Interner>>,
    sum_error: Arc<Mutex<Option<String>>>,
    constructors: Arc<BTreeMap<RelId, (String, Vec<Ty>)>>,
    texts: Arc<Vec<Cell>>,
    terms: Arc<Vec<Cell>>,
}

/// Concatenation: the group's rows, whatever order the pieces arrive in.
#[derive(Clone)]
struct Concat;

impl dbsp::algebra::Semigroup<Group> for Concat {
    fn combine(left: &Group, right: &Group) -> Group {
        let mut all = left.clone();
        all.extend_from_slice(right);
        all
    }
}

/// One scope's operator builder. `C` is `RootCircuit` or the `NestedCircuit` of one LetRec.
pub struct DbspRel<C: dbsp::Circuit> {
    circuit: C,
    shared: Shared,
    sources: BTreeMap<RelId, Z<C>>,
    outputs: Vec<(RelId, Z<C>)>,
    /// Relations `get` handed a stream for.
    read: BTreeSet<RelId>,
    types: Vec<Option<Option<Vec<Ty>>>>,
    /// The first error a LetRec closure hit; `recursive_dynamic` takes no `EngineError`.
    error: Option<EngineError>,
}

impl<C: dbsp::Circuit> DbspRel<C> {
    fn new(circuit: C, shared: Shared) -> Self {
        Self { circuit, shared, sources: BTreeMap::new(), outputs: Vec::new(), read: BTreeSet::new(), types: Vec::new(), error: None }
    }
}

/// Cells of `key` from `row`; an `Any` cell is replaced by its comparison key, and a NULL `Any`
/// cell drops the row (SQL equality never matches NULL).
fn key_cells(interner: &Interner, row: &[Cell], key: &[ColId], any: &[bool]) -> Option<Row> {
    let mut cells = cols(row, key);
    for (cell, any) in cells.iter_mut().zip(any) {
        if *any {
            if matches!(interner.any_value(*cell), Some(AnyValue::Null)) { return None; }
            *cell = interner.any_key(*cell)?;
        }
    }
    Some(cells)
}

fn mfp_row(shared: &Shared, filter: &[Expr], map: &[Expr], project: &[ColId], input_types: &[Ty], mut row: Row) -> Option<Row> {
    let interner = shared.interner.lock().unwrap();
    let lt = |a, b| interner.compare(a, b) == Ordering::Less;
    let literal = |index: u32| shared.texts[index as usize];
    let term = |index: u32| shared.terms[index as usize];
    let nil = || interner.text_id("").expect("empty string");
    let compare = |ta: Ty, a: Cell, tb: Ty, b: Cell| {
        let numeric = |ty: Ty| matches!(ty, Ty::Int | Ty::Real);
        if numeric(ta) && numeric(tb) {
            if ta == Ty::Int && tb == Ty::Int { return a.cmp(&b); }
            let x = if ta == Ty::Real { f64::from_bits(a as u64) } else { a as f64 };
            let y = if tb == Ty::Real { f64::from_bits(b as u64) } else { b as f64 };
            return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
        }
        let rank = |ty: Ty| match ty { Ty::Int | Ty::Real => 1, Ty::Text => 2, Ty::Id => 3, Ty::Any => 4 };
        rank(ta).cmp(&rank(tb)).then_with(|| cmp_cell(ta, a, b, &interner))
    };
    if !filter.iter().all(|e| eval_typed_with_text(e, &row, input_types, &lt, &literal, &term, &nil, &compare) != 0) {
        return None;
    }
    let mut types = input_types.to_vec();
    for e in map {
        let v = eval_typed_with_text(e, &row, &types, &lt, &literal, &term, &nil, &compare);
        types.push(expr_type(e, &types).expect("Mfp expression type"));
        row.push(v);
    }
    Some(if project.is_empty() { row } else { cols(&row, project) })
}

fn mint_row(shared: &Shared, name: &str, types: &[Ty], columns: &[ColId], mut row: Row) -> Row {
    let values = cols(&row, columns);
    let id = shared.interner.lock().unwrap().mint(name, &values, types);
    row.push(id);
    row
}

fn decode_row(shared: &Shared, name: &str, col: ColId, mut row: Row) -> Option<Row> {
    let id = *row.get(col as usize)?;
    let interner = shared.interner.lock().unwrap();
    let term = interner.get(id).filter(|term| *term.functor == *name)?;
    row.push(id);
    row.extend_from_slice(&term.args);
    Some(row)
}

fn str_cons_row(shared: &Shared, mode: &StrMode, mut row: Row) -> Option<Row> {
    let mut dict = shared.interner.lock().unwrap();
    match mode {
        StrMode::Construct { head, rest } => {
            let joined = format!("{}{}", dict.text(row[*head as usize])?, dict.text(row[*rest as usize])?);
            row.push(dict.mint_text(&joined));
        }
        StrMode::Decompose { whole } => {
            // Head and rest are minted here, when a row reads them; the empty string emits no row.
            let text = dict.text(row[*whole as usize])?;
            let at = text.chars().next()?.len_utf8();
            let (head, rest) = (text[..at].to_owned(), text[at..].to_owned());
            row.extend([dict.mint_text(&head), dict.mint_text(&rest)]);
        }
    }
    Some(row)
}

fn str_op_row(shared: &Shared, op: StrOp, columns: &[ColId], mut row: Row) -> Option<Row> {
    let mut dict = shared.interner.lock().unwrap();
    let out = {
        let mut values = Vec::with_capacity(columns.len());
        for (column, kind) in columns.iter().zip(op.args()) {
            let cell = row[*column as usize];
            values.push(match kind {
                StrKind::Text => StrVal::Text(dict.text(cell)?),
                StrKind::Int => StrVal::Int(cell),
            });
        }
        op.apply(&values)?
    };
    match out {
        StrOut::Text(text) => row.push(dict.mint_text(&text)),
        StrOut::Int(value) => row.push(value),
        StrOut::Holds => {}
    }
    Some(row)
}

/// `Reduce` over one group's rows: `key` columns of the least live row, then each aggregate.
fn reduce_group(shared: &Shared, key: &[ColId], aggs: &[Agg], types: &[Ty], rows: &[(Row, W)]) -> Group {
    let count: W = rows.iter().map(|(_, w)| *w).sum();
    if count <= 0 { return Vec::new(); }
    let interner = shared.interner.lock().unwrap();
    let live: Vec<&Row> = rows.iter().filter(|(_, w)| *w > 0).map(|(row, _)| row).collect();
    let Some(representative) = live.iter().min_by_key(|row| cols(row, key)) else { return Vec::new() };
    let mut out = cols(representative, key);
    let extreme = |col: ColId, max: bool| {
        let ty = types[col as usize];
        let values = live.iter().map(|row| row[col as usize])
            .filter(|cell| ty != Ty::Any || !matches!(interner.any_value(*cell), Some(AnyValue::Null)));
        let pick = if max { values.max_by(|a, b| cmp_cell(ty, *a, *b, &interner)) } else { values.min_by(|a, b| cmp_cell(ty, *a, *b, &interner)) };
        pick.unwrap_or(live[0][col as usize])
    };
    let mut any_sums = Vec::new();
    for agg in aggs {
        out.push(match agg {
            Agg::Count => count,
            Agg::Sum(col) if types[*col as usize] == Ty::Real => rows.iter()
                .map(|(row, w)| f64::from_bits(row[*col as usize] as u64) * *w as f64)
                .sum::<f64>().to_bits() as i64,
            Agg::Sum(col) if types[*col as usize] == Ty::Any => {
                let values = rows.iter().map(|(row, w)| (interner.any_value(row[*col as usize]).expect("Any cell").clone(), *w)).collect::<Vec<_>>();
                any_sums.push((out.len(), values));
                0
            }
            Agg::Sum(col) => rows.iter().fold(0i64, |sum, (row, w)| sum.wrapping_add(row[*col as usize].wrapping_mul(*w))),
            Agg::Min(col) => extreme(*col, false),
            Agg::Max(col) => extreme(*col, true),
        });
    }
    drop(interner);
    for (at, values) in any_sums {
        let sum = match sqlite_sum_any(&values) {
            Ok(sum) => sum,
            Err(error) => {
                *shared.sum_error.lock().unwrap() = Some(error.to_string());
                AnyValue::Null
            }
        };
        out[at] = shared.interner.lock().unwrap().mint_any(&sum);
    }
    vec![(out, 1)]
}

fn topk_group(shared: &Shared, order: &[Order], types: &[Ty], limit: u32, rows: &[(Row, W)]) -> Group {
    let interner = shared.interner.lock().unwrap();
    let mut live: Vec<(&Row, W)> = rows.iter().filter(|(_, w)| *w > 0).map(|(r, w)| (r, *w)).collect();
    live.sort_by(|(a, _), (b, _)| rank(order, types, &interner, a, b));
    let mut left = limit as W;
    let mut out = Vec::new();
    for (row, w) in live {
        if left == 0 { break; }
        let take = w.min(left);
        out.push((row.clone(), take));
        left -= take;
    }
    out
}

fn window_group(shared: &Shared, order: &[Order], func: &WinFn, types: &[Ty], rows: &[(Row, W)]) -> Group {
    let interner = shared.interner.lock().unwrap();
    let value_col = order.first().map_or(0, |o| o.col as usize);
    let mut all = Vec::new();
    for (row, weight) in rows {
        for _ in 0..(*weight).max(0) {
            all.push(row.clone());
        }
    }
    all.sort_by(|a, b| rank(order, types, &interner, a, b));
    let total_sum = if let WinFn::Sum(col) = func {
        all.iter().fold(0i64, |sum, row| sum.wrapping_add(row[*col as usize]))
    } else { 0 };
    let mut values: BTreeMap<Row, W> = BTreeMap::new();
    let (mut dense, mut rank_at, mut prefix_sum) = (0i64, 0i64, 0i64);
    for (i, row) in all.iter().enumerate() {
        if i == 0 || order.iter().any(|o| row[o.col as usize] != all[i - 1][o.col as usize]) {
            dense += 1;
            rank_at = i as i64 + 1;
        }
        let value = match func {
            WinFn::RowNumber => i as i64 + 1,
            WinFn::Rank => rank_at,
            WinFn::DenseRank => dense,
            WinFn::Lag(offset) => i.checked_sub(*offset as usize).map_or(0, |j| all[j][value_col]),
            WinFn::Lead(offset) => all.get(i + *offset as usize).map_or(0, |r| r[value_col]),
            WinFn::Sum(col) => {
                prefix_sum = prefix_sum.wrapping_add(row[*col as usize]);
                if order.is_empty() { total_sum } else { prefix_sum }
            }
            WinFn::Count => if order.is_empty() { all.len() as i64 } else { i as i64 + 1 },
        };
        let mut result = row.clone();
        result.push(value);
        *values.entry(result).or_default() += 1;
    }
    values.into_iter().collect()
}

/// Rows each with its weight as that many copies: `flat_map` consolidates them back.
fn spread(group: &Group) -> Vec<Row> {
    let mut rows = Vec::new();
    for (row, w) in group {
        for _ in 0..(*w).max(0) {
            rows.push(row.clone());
        }
    }
    rows
}

fn fold_group() -> impl Fn(&mut Group, &Row, ZWeight) + Clone + 'static {
    |acc: &mut Group, row: &Row, w: ZWeight| acc.push((row.clone(), w))
}

/// The operator algebra over `$circuit`. dbsp's monomorphic API names each circuit type
/// separately, so one body is stamped out for the root and for a LetRec's child circuit.
macro_rules! rel_impl {
    ($circuit:ident) => {
        impl DbspRel<$circuit> {
            fn keyed(&mut self, c: Z<$circuit>, key: Vec<ColId>, any: Vec<bool>) -> Keyed<$circuit> {
                let shared = self.shared.clone();
                c.flat_map_index(move |row: &Row| {
                    let cells = key_cells(&shared.interner.lock().unwrap(), row, &key, &any);
                    cells.map(|cells| (cells, row.clone()))
                })
            }

            fn join_rows(&mut self, cs: Vec<Z<$circuit>>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>], project: Option<Vec<ColId>>) -> Result<Z<$circuit>, EngineError> {
                if cs.len() != 2 {
                    return Err(EngineError::new(Stage::Install, None, ErrorKind::Unsupported("Join arity != 2")));
                }
                let side = |input: u8| -> Vec<ColId> {
                    eq.iter()
                        .map(|class| class.iter().find(|(i, _)| *i == input).map(|(_, c)| *c).expect("check: class spans both inputs"))
                        .collect()
                };
                let (lk, rk) = (side(0), side(1));
                let l_any = lk.iter().map(|c| types[0][*c as usize] == Ty::Any).collect::<Vec<_>>();
                let r_any = rk.iter().map(|c| types[1][*c as usize] == Ty::Any).collect::<Vec<_>>();
                let mut cs = cs.into_iter();
                let (l, r) = (cs.next().unwrap(), cs.next().unwrap());
                let left = types[0].len();
                let l = self.keyed(l, lk, l_any);
                let r = self.keyed(r, rk, r_any);
                Ok(l.join(&r, move |_, a: &Row, b: &Row| join_row(a, b, left, &project)))
            }
        }

        impl Rel for DbspRel<$circuit> {
            type C = Z<$circuit>;

            fn get(&mut self, rel: RelId) -> Result<Self::C, EngineError> {
                self.read.insert(rel);
                self.sources.get(&rel).cloned()
                    .ok_or_else(|| EngineError::new(Stage::Install, Some(rel), ErrorKind::UnknownRel(rel)))
            }

            fn mint(&mut self, c: Self::C, functor: RelId, args: &[ColId]) -> Result<Self::C, EngineError> {
                let (name, types) = self.shared.constructors.get(&functor)
                    .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
                if args.len() != types.len() {
                    return Err(EngineError::new(Stage::Install, Some(functor), ErrorKind::Arity { expected: types.len(), actual: args.len() }));
                }
                let (columns, shared) = (args.to_vec(), self.shared.clone());
                Ok(c.map(move |row: &Row| mint_row(&shared, &name, &types, &columns, row.clone())))
            }

            fn decode(&mut self, c: Self::C, functor: RelId, col: ColId, _types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
                let (name, _) = self.shared.constructors.get(&functor)
                    .ok_or_else(|| EngineError::new(Stage::Install, Some(functor), ErrorKind::UnknownRel(functor)))?.clone();
                let shared = self.shared.clone();
                Ok(c.flat_map(move |row: &Row| decode_row(&shared, &name, col, row.clone())))
            }

            fn str_cons(&mut self, c: Self::C, mode: &StrMode) -> Result<Self::C, EngineError> {
                let (mode, shared) = (mode.clone(), self.shared.clone());
                Ok(c.flat_map(move |row: &Row| str_cons_row(&shared, &mode, row.clone())))
            }

            fn str_op(&mut self, c: Self::C, op: StrOp, args: &[ColId]) -> Result<Self::C, EngineError> {
                if args.len() != op.args().len() {
                    return Err(EngineError::new(Stage::Install, None, ErrorKind::Arity { expected: op.args().len(), actual: args.len() }));
                }
                let (columns, shared) = (args.to_vec(), self.shared.clone());
                Ok(c.flat_map(move |row: &Row| str_op_row(&shared, op, &columns, row.clone())))
            }

            fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId], input_types: &[Ty]) -> Self::C {
                let (filter, map, project, input_types) = (filter.to_vec(), map.to_vec(), project.to_vec(), input_types.to_vec());
                let shared = self.shared.clone();
                c.flat_map(move |row: &Row| mfp_row(&shared, &filter, &map, &project, &input_types, row.clone()))
            }

            fn union(&mut self, cs: Vec<Self::C>) -> Self::C {
                let mut cs = cs.into_iter();
                let first = cs.next().expect("check rejects an empty Union");
                let rest: Vec<Self::C> = cs.collect();
                if rest.is_empty() { first } else { first.sum(rest.iter()) }
            }

            fn negate(&mut self, c: Self::C) -> Self::C {
                c.neg()
            }

            fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>]) -> Result<Self::C, EngineError> {
                self.join_rows(cs, eq, types, None)
            }

            fn fuses_join_project(&self) -> bool {
                true
            }

            fn join_project(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>], types: &[Vec<Ty>], project: &[ColId]) -> Result<Self::C, EngineError> {
                self.join_rows(cs, eq, types, Some(project.to_vec()))
            }

            /// `l` minus its semijoin with `r`'s live key set.
            fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C {
                let (lk, rk) = (lk.to_vec(), rk.to_vec());
                let keys = r.map(move |row: &Row| cols(row, &rk)).distinct().map_index(|key: &Row| (key.clone(), ()));
                l.map_index(move |row: &Row| (cols(row, &lk), row.clone()))
                    .antijoin(&keys)
                    .map(|(_, row): (&Row, &Row)| row.clone())
            }

            fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg], input_types: &[Ty]) -> Self::C {
                let (key, aggs, types) = (key.to_vec(), aggs.to_vec(), input_types.to_vec());
                let shared = self.shared.clone();
                let group_of = {
                    let (key, types, shared) = (key.clone(), types.clone(), shared.clone());
                    move |row: &Row| {
                        let mut group = cols(row, &key);
                        for (cell, col) in group.iter_mut().zip(&key) {
                            if types[*col as usize] == Ty::Any {
                                *cell = shared.interner.lock().unwrap().any_key(*cell).expect("Any cell");
                            }
                        }
                        (group, row.clone())
                    }
                };
                c.map_index(group_of)
                    .aggregate(Fold::<Row, Group, Concat, _, _>::with_output(Vec::new(), fold_group(),
                        move |rows: Group| reduce_group(&shared, &key, &aggs, &types, &rows)))
                    .flat_map(|(_, group): (&Row, &Group)| spread(group))
            }

            fn threshold(&mut self, c: Self::C) -> Self::C {
                c.distinct()
            }

            fn topk(&mut self, c: Self::C, key: &[ColId], order: &[Order], limit: u32, input_types: &[Ty]) -> Result<Self::C, EngineError> {
                let (key, order, types) = (key.to_vec(), order.to_vec(), input_types.to_vec());
                let shared = self.shared.clone();
                Ok(c.map_index(move |row: &Row| (cols(row, &key), row.clone()))
                    .aggregate(Fold::<Row, Group, Concat, _, _>::with_output(Vec::new(), fold_group(),
                        move |rows: Group| topk_group(&shared, &order, &types, limit, &rows)))
                    .flat_map(|(_, group): (&Row, &Group)| spread(group)))
            }

            fn window(&mut self, c: Self::C, partition: &[ColId], order: &[Order], func: &WinFn, input_types: &[Ty]) -> Result<Self::C, EngineError> {
                let (partition, order, func, types) = (partition.to_vec(), order.to_vec(), func.clone(), input_types.to_vec());
                let shared = self.shared.clone();
                Ok(c.map_index(move |row: &Row| (cols(row, &partition), row.clone()))
                    .aggregate(Fold::<Row, Group, Concat, _, _>::with_output(Vec::new(), fold_group(),
                        move |rows: Group| window_group(&shared, &order, &func, &types, &rows)))
                    .flat_map(|(_, group): (&Row, &Group)| spread(group)))
            }

            fn letrec(&mut self, p: &Program, rec: &LetRec, defined: &[(RelId, Self::C)], outer: &mut Vec<Option<Self::C>>) -> Result<Vec<Self::C>, EngineError> {
                letrec_at!(self, p, rec, defined, outer, $circuit)
            }

            fn node_types(&mut self, p: &Program, id: NodeId) -> Option<Vec<Ty>> {
                p.node_types_memo(id, &mut self.types)
            }

            fn output(&mut self, rel: RelId, c: Self::C) {
                self.outputs.push((rel, c));
            }
        }
    };
}

/// A LetRec reading none of its own variables is its bodies, thresholded, in this scope.
fn letrec_shortcut<A: Rel>(rel: &mut A, p: &Program, rec: &LetRec, defined: &[(RelId, A::C)], outer: &mut Vec<Option<A::C>>) -> Option<Result<Vec<A::C>, EngineError>> {
    if !rec.nested.is_empty() || rec_inputs(p, rec).iter().any(|id| rec.ids.contains(id)) {
        return None;
    }
    Some(rec.bodies.iter().map(|body| {
        let c = lower_node(p, rel, outer, defined, *body)?;
        Ok(rel.threshold(c))
    }).collect())
}

macro_rules! letrec_at {
    ($self:ident, $p:ident, $rec:ident, $defined:ident, $outer:ident, RootCircuit) => {{
        if let Some(done) = letrec_shortcut($self, $p, $rec, $defined, $outer) {
            return done;
        }
        root_letrec($self, $p, $rec, $defined)
    }};
    ($self:ident, $p:ident, $rec:ident, $defined:ident, $outer:ident, NestedCircuit) => {{
        if let Some(done) = letrec_shortcut($self, $p, $rec, $defined, $outer) {
            return done;
        }
        Err(EngineError::new(Stage::Install, $rec.ids.first().copied(), ErrorKind::Unsupported("LetRec inside a LetRec")))
    }};
}

rel_impl!(RootCircuit);
rel_impl!(NestedCircuit);

/// One LetRec as a `recursive_dynamic` child circuit. Every relation the bodies read that is
/// no variable enters through `delta0`; the child applies its own `distinct` to each variable.
fn root_letrec(rel: &mut DbspRel<RootCircuit>, p: &Program, rec: &LetRec, defined: &[(RelId, Z<RootCircuit>)]) -> Result<Vec<Z<RootCircuit>>, EngineError> {
    if !rec.nested.is_empty() {
        return Err(EngineError::new(Stage::Install, rec.ids.first().copied(), ErrorKind::Unsupported("nested LetRec")));
    }
    if rec.limit.is_some() {
        return Err(EngineError::new(Stage::Install, rec.ids.first().copied(), ErrorKind::Unsupported("LetRec limit")));
    }
    let used = rec_inputs(p, rec);
    // Outer streams the bodies read, resolved before the child circuit borrows the root.
    let mut imports: Vec<(RelId, Z<RootCircuit>, bool)> = Vec::new();
    for id in &used {
        if rec.ids.contains(id) { continue; }
        if let Some((_, c)) = defined.iter().find(|(d, _)| d == id) {
            imports.push((*id, c.clone(), true));
        } else {
            imports.push((*id, rel.get(*id)?, false));
        }
    }
    let shared = rel.shared.clone();
    let mut failure: Option<EngineError> = None;
    let results = rel.circuit.recursive_dynamic(rec.ids.len(), |child: &NestedCircuit, vars: Vec<Z<NestedCircuit>>| {
        let mut inner = DbspRel::new(child.clone(), shared.clone());
        let mut scope_defined: Vec<(RelId, Z<NestedCircuit>)> = rec.ids.iter().copied().zip(vars.iter().cloned()).collect();
        for (id, c, is_defined) in &imports {
            let entered = c.delta0(child);
            if *is_defined { scope_defined.push((*id, entered)); } else { inner.sources.insert(*id, entered); }
        }
        let mut nodes = vec![None; p.nodes.len()];
        let mut bodies = Vec::with_capacity(rec.bodies.len());
        for body in &rec.bodies {
            match lower_node(p, &mut inner, &mut nodes, &scope_defined, *body) {
                Ok(c) => bodies.push(c),
                Err(e) => {
                    failure = Some(e);
                    return Ok(vars);
                }
            }
        }
        Ok(bodies)
    }).map_err(|e| EngineError::new(Stage::Install, rec.ids.first().copied(), ErrorKind::Worker(e.to_string())))?;
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(results)
}

/// What the constructor hands back to the caller's thread.
struct Built {
    inputs: BTreeMap<RelId, ZSetHandle<Row>>,
    constructor_inputs: BTreeMap<RelId, ZSetHandle<Row>>,
    outputs: Vec<(RelId, OutputHandle<OrdZSet<Row>>)>,
}

pub struct Dbsp {
    handle: Option<dbsp::DBSPHandle>,
    built: Built,
    shared: Shared,
    program: Program,
    constructors: BTreeMap<RelId, (String, Vec<Ty>)>,
    constructor_ids: HashMap<String, RelId>,
    source_rows: BTreeMap<RelId, HashSet<Row>>,
    /// Each output's rows, accumulated from the settled deltas; `snapshot` reads them.
    totals: BTreeMap<RelId, BTreeMap<Row, W>>,
    tick: u64,
    counters: Counters,
}

impl Dbsp {
    fn feed_pending(&mut self) -> bool {
        let mut fed = false;
        for (name, row) in self.shared.interner.lock().unwrap().drain_pending() {
            if let Some(input) = self.constructor_ids.get(&*name).and_then(|id| self.built.constructor_inputs.get(id)) {
                input.push(row, 1);
                fed = true;
            }
        }
        fed
    }

    fn step(&mut self, net: &mut BTreeMap<(RelId, Row), W>) -> Result<(), EngineError> {
        self.handle.as_mut().expect("live circuit").transaction()
            .map_err(|e| EngineError::new(Stage::Settle, None, ErrorKind::Worker(e.to_string())))?;
        for (rel, output) in &self.built.outputs {
            for (row, (), w) in output.consolidate().iter() {
                *net.entry((*rel, row)).or_default() += w;
            }
        }
        Ok(())
    }
}

impl Engine for Dbsp {
    fn install(program: &Program) -> Result<Self, EngineError> {
        let interner = Arc::new(Mutex::new(Interner::default()));
        let constructors: BTreeMap<RelId, (String, Vec<Ty>)> = program.rels.iter()
            .filter(|r| r.kind == RelKind::Constructor)
            .map(|r| (r.id, (r.name.clone(), r.cols.iter().skip(1).copied().collect())))
            .collect();
        let mut constructor_ids: HashMap<String, RelId> = HashMap::new();
        for (id, (name, _)) in &constructors {
            constructor_ids.entry(name.clone()).or_insert(*id);
        }
        let (texts, terms) = {
            let mut dict = interner.lock().unwrap();
            if program.uses_strings() { dict.mint_text(""); }
            let texts: Vec<Cell> = program.texts.iter().map(|text| dict.mint_text(text)).collect();
            // Each argument term is minted before the term naming it; the minted rows enter the
            // constructor inputs with the first settle.
            let mut terms: Vec<Cell> = Vec::with_capacity(program.terms.len());
            for lit in &program.terms {
                let (name, types) = constructors.get(&lit.functor)
                    .ok_or_else(|| EngineError::new(Stage::Install, Some(lit.functor), ErrorKind::UnknownRel(lit.functor)))?;
                if lit.args.len() != types.len() {
                    return Err(EngineError::new(Stage::Install, Some(lit.functor), ErrorKind::Arity { expected: types.len(), actual: lit.args.len() }));
                }
                let args = lit.args.iter().map(|arg| match arg {
                    TermArg::Term(index) => terms.get(*index as usize).copied(),
                    TermArg::Text(index) => texts.get(*index as usize).copied(),
                    TermArg::Raw(cell) => Some(*cell),
                }).collect::<Option<Vec<Cell>>>()
                    .ok_or_else(|| EngineError::new(Stage::Install, Some(lit.functor), ErrorKind::Unsupported("term argument after its term")))?;
                terms.push(dict.mint(name, &args, types));
            }
            (texts, terms)
        };
        let shared = Shared {
            interner,
            sum_error: Arc::default(),
            constructors: Arc::new(constructors.clone()),
            texts: Arc::new(texts),
            terms: Arc::new(terms),
        };
        let build_shared = shared.clone();
        let build_program = program.clone();
        let (handle, built) = Runtime::init_circuit(1, move |circuit: &mut RootCircuit| {
            let mut rel = DbspRel::new(circuit.clone(), build_shared);
            let mut inputs = BTreeMap::new();
            let mut constructor_inputs = BTreeMap::new();
            for r in build_program.rels.iter().filter(|r| r.kind == RelKind::Source) {
                let (stream, input) = circuit.add_input_zset::<Row>();
                inputs.insert(r.id, input);
                rel.sources.insert(r.id, stream);
            }
            for r in build_program.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
                let (stream, input) = circuit.add_input_zset::<Row>();
                constructor_inputs.insert(r.id, input);
                rel.sources.insert(r.id, stream);
            }
            if let Err(e) = lower(&build_program, &mut rel) {
                return Ok(Err(e));
            }
            if let Some(e) = rel.error.take() {
                return Ok(Err(e));
            }
            // A constructor no operator reads gets no rows: its terms would cost a step and drive nothing.
            constructor_inputs.retain(|id, _| rel.read.contains(id));
            let outputs = rel.outputs.iter().map(|(id, c)| (*id, c.output())).collect();
            Ok(Ok(Built { inputs, constructor_inputs, outputs }))
        }).map_err(|e| EngineError::new(Stage::Install, None, ErrorKind::Worker(e.to_string())))?;
        let built = built?;
        let source_rows = program.rels.iter()
            .filter(|rel| rel.kind == RelKind::Source)
            .map(|rel| (rel.id, HashSet::new())).collect();
        let totals = built.outputs.iter().map(|(id, _)| (*id, BTreeMap::new())).collect();
        Ok(Self { handle: Some(handle), built, shared, program: program.clone(), constructors, constructor_ids, source_rows, totals, tick: 0, counters: Counters::default() })
    }

    fn settle(&mut self, frontier: Frontier) -> Result<Delta, EngineError> {
        let interned_before = self.shared.interner.lock().unwrap().len();
        let accepted = guard(&self.program, &mut self.source_rows, self.tick, &frontier)?;
        for change in accepted {
            self.built.inputs.get(&change.rel).expect("guard admits sources only").push(change.row, change.w);
        }
        // Terms interned since the last settle enter with the frontier.
        self.feed_pending();
        let mut net: BTreeMap<(RelId, Row), W> = BTreeMap::new();
        self.step(&mut net)?;
        if let Some(message) = self.shared.sum_error.lock().unwrap().take() {
            return Err(EngineError::new(Stage::Settle, None, ErrorKind::Worker(message)));
        }
        // Terms minted during a step drive a further step, until none are new.
        while self.feed_pending() {
            self.step(&mut net)?;
        }
        let changes: Vec<(RelId, Row, W)> = net.into_iter()
            .filter(|(_, w)| *w != 0)
            .map(|((rel, row), w)| (rel, row, w))
            .collect();
        for (rel, row, w) in &changes {
            let rows = self.totals.get_mut(rel).expect("an output");
            let total = rows.entry(row.clone()).or_default();
            *total += w;
            if *total == 0 {
                rows.remove(row);
            }
        }
        self.counters = Counters {
            rows_written: changes.len() as u64,
            interned: Some((self.shared.interner.lock().unwrap().len() - interned_before) as u64),
            ..Counters::default()
        };
        let delta = Delta { tick: self.tick, changes };
        self.tick += 1;
        Ok(delta)
    }

    fn counters(&self) -> Counters { self.counters }

    fn snapshot(&self, rel: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        self.totals.get(&rel)
            .map(|rows| rows.iter().map(|(row, w)| (row.clone(), *w)).collect())
            .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel)))
    }

    fn intern_snapshot(&self, functor: RelId) -> Result<Vec<(Row, W)>, EngineError> {
        self.constructors.get(&functor)
            .map(|(name, _)| self.shared.interner.lock().unwrap().snapshot(name))
            .ok_or_else(|| EngineError::new(Stage::Snapshot, Some(functor), ErrorKind::UnknownRel(functor)))
    }

    fn intern_terms(&mut self, terms: &[(RelId, Row)]) -> Result<Vec<Cell>, EngineError> {
        terms.iter().map(|(functor, args)| {
            let (name, types) = self.constructors.get(functor)
                .ok_or_else(|| EngineError::new(Stage::Settle, Some(*functor), ErrorKind::UnknownRel(*functor)))?;
            if args.len() != types.len() {
                return Err(EngineError::new(Stage::Settle, Some(*functor), ErrorKind::Arity { expected: types.len(), actual: args.len() }));
            }
            Ok(self.shared.interner.lock().unwrap().mint(name, args, types))
        }).collect()
    }

    fn declare_constructors(&mut self, ctors: &[(String, Vec<Ty>)]) -> Result<Vec<RelId>, EngineError> {
        let mut next = self.program.rels.iter().map(|rel| rel.id + 1).chain(self.constructors.keys().map(|id| id + 1)).max().unwrap_or(0);
        Ok(ctors.iter().cloned().map(|(name, types)| match self.constructor_ids.get(&name) {
            Some(id) => *id,
            None => {
                let id = next;
                next += 1;
                self.constructor_ids.insert(name.clone(), id);
                self.constructors.insert(id, (name, types));
                id
            }
        }).collect())
    }

    fn intern_text(&mut self, text: &str) -> Result<Cell, EngineError> {
        Ok(self.shared.interner.lock().unwrap().mint_text(text))
    }

    fn text(&self, id: Cell) -> Result<Option<String>, EngineError> {
        Ok(self.shared.interner.lock().unwrap().text(id).map(str::to_owned))
    }

    fn intern_any(&mut self, value: &AnyValue) -> Result<Cell, EngineError> {
        Ok(self.shared.interner.lock().unwrap().mint_any(value))
    }

    fn any_value(&self, id: Cell) -> Result<AnyValue, EngineError> {
        self.shared.interner.lock().unwrap().any_value(id).cloned()
            .ok_or_else(|| EngineError::new(Stage::Snapshot, None, ErrorKind::Worker(format!("unknown Any cell {id}"))))
    }
}

impl Drop for Dbsp {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.kill();
        }
    }
}
