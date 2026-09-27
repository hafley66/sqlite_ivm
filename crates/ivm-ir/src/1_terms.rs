use crate::{Cell, Row, Ty};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub functor: String,
    pub args: Row,
    pub types: Vec<Ty>,
}

#[derive(Default)]
pub struct Interner {
    by_key: BTreeMap<(String, Row), Cell>,
    by_id: BTreeMap<Cell, Term>,
    next: Cell,
    pending: Vec<(String, Row)>,
}

impl Interner {
    pub fn mint(&mut self, functor: &str, args: &[Cell], types: &[Ty]) -> Cell {
        let key = (functor.to_owned(), args.to_vec());
        if let Some(id) = self.by_key.get(&key) {
            return *id;
        }
        let id = self.next.checked_add(1).expect("term ID space exhausted");
        self.next = id;
        self.by_key.insert(key, id);
        self.by_id.insert(id, Term { functor: functor.to_owned(), args: args.to_vec(), types: types.to_vec() });
        let mut row = vec![id];
        row.extend_from_slice(args);
        self.pending.push((functor.to_owned(), row));
        id
    }

    pub fn drain_pending(&mut self) -> Vec<(String, Row)> {
        std::mem::take(&mut self.pending)
    }

    pub fn get(&self, id: Cell) -> Option<&Term> {
        self.by_id.get(&id)
    }

    pub fn snapshot(&self, functor: &str) -> Vec<(Row, i64)> {
        self.by_id.iter().filter(|(_, term)| term.functor == functor).map(|(id, term)| {
            let mut row = vec![*id];
            row.extend(&term.args);
            (row, 1)
        }).collect()
    }

    pub fn compare(&self, a: Cell, b: Cell) -> Ordering {
        compare(a, b, &mut |id| self.get(id).cloned())
    }
}

/// Non-dictionary IDs are atomic symbols ordered by their integer value.
pub fn compare(a: Cell, b: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>) -> Ordering {
    if a == b { return Ordering::Equal; }
    match (resolve(a), resolve(b)) {
        (None, None) => a.cmp(&b),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.args.len().cmp(&y.args.len())
            .then_with(|| x.functor.cmp(&y.functor))
            .then_with(|| {
                for ((a, ta), (b, tb)) in x.args.iter().zip(&x.types).zip(y.args.iter().zip(&y.types)) {
                    let order = match (ta, tb) {
                        (Ty::Id, Ty::Id) => compare(*a, *b, resolve),
                        _ => (*ta as u8).cmp(&(*tb as u8)).then_with(|| a.cmp(b)),
                    };
                    if order != Ordering::Equal { return order; }
                }
                Ordering::Equal
            }),
    }
}

/// A byte key whose lexical order matches `compare` for acyclic terms.
pub fn sort_key(id: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>) -> Vec<u8> {
    fn number(out: &mut Vec<u8>, value: Cell) {
        out.extend_from_slice(&((value as u64) ^ (1u64 << 63)).to_be_bytes());
    }
    fn key(out: &mut Vec<u8>, id: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>, depth: usize) {
        if depth >= 128 {
            out.push(0);
            number(out, id);
            return;
        }
        let Some(term) = resolve(id) else {
            out.push(0);
            number(out, id);
            return;
        };
        out.push(1);
        out.extend_from_slice(&(term.args.len() as u32).to_be_bytes());
        for b in term.functor.bytes() {
            if b == 0 { out.extend_from_slice(&[0, 255]); } else { out.push(b); }
        }
        out.extend_from_slice(&[0, 0]);
        for (arg, ty) in term.args.into_iter().zip(term.types) {
            match ty {
                Ty::Int => { out.push(0); number(out, arg); }
                Ty::Id => { out.push(1); key(out, arg, resolve, depth + 1); }
            }
        }
    }
    let mut out = Vec::new();
    key(&mut out, id, resolve, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_terms_compare_by_fields_independent_of_insertion_ids() {
        let mut terms = Interner::default();
        let larger = terms.mint("pair", &[1, 3], &[Ty::Int, Ty::Int]);
        let smaller = terms.mint("pair", &[1, 2], &[Ty::Int, Ty::Int]);
        let wrapped_larger = terms.mint("wrap", &[larger], &[Ty::Id]);
        let wrapped_smaller = terms.mint("wrap", &[smaller], &[Ty::Id]);
        assert_eq!(terms.mint("pair", &[1, 2], &[Ty::Int, Ty::Int]), smaller);
        assert_eq!(terms.compare(wrapped_smaller, wrapped_larger), Ordering::Less);
        assert!(sort_key(wrapped_smaller, &mut |id| terms.get(id).cloned())
            < sort_key(wrapped_larger, &mut |id| terms.get(id).cloned()));
    }
}
