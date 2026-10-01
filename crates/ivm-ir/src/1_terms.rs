use crate::{AnyValue, Cell, Row, Ty};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub functor: String,
    pub args: Row,
    pub types: Vec<Ty>,
    pub text: Option<String>,
    pub split: Option<(Cell, Cell)>,
}

#[derive(Default)]
pub struct Interner {
    by_key: BTreeMap<(String, Row), Cell>,
    by_id: BTreeMap<Cell, Term>,
    by_text: BTreeMap<String, Cell>,
    by_any: BTreeMap<AnyValue, Cell>,
    any_by_id: BTreeMap<Cell, AnyValue>,
    next: Cell,
    pending: Vec<(String, Row)>,
}

impl Interner {
    pub fn len(&self) -> usize { self.by_id.len() + self.any_by_id.len() }

    pub fn mint(&mut self, functor: &str, args: &[Cell], types: &[Ty]) -> Cell {
        let key = (functor.to_owned(), args.to_vec());
        if let Some(id) = self.by_key.get(&key) {
            return *id;
        }
        let id = self.next.checked_add(1).expect("term ID space exhausted");
        self.next = id;
        self.by_key.insert(key, id);
        self.by_id.insert(id, Term { functor: functor.to_owned(), args: args.to_vec(), types: types.to_vec(), text: None, split: None });
        let mut row = vec![id];
        row.extend_from_slice(args);
        self.pending.push((functor.to_owned(), row));
        id
    }

    pub fn mint_text(&mut self, text: &str) -> Cell {
        if let Some(id) = self.by_text.get(text) { return *id; }
        let split = if let Some(first) = text.chars().next() {
            let (_, rest) = text.split_at(first.len_utf8());
            let rest_id = self.mint_text(rest);
            if rest.is_empty() { Some((0, rest_id)) } else {
                Some((self.mint_text(&text[..first.len_utf8()]), rest_id))
            }
        } else { None };
        let id = self.next.checked_add(1).expect("term ID space exhausted");
        self.next = id;
        let split = split.map(|(head, rest)| (if head == 0 { id } else { head }, rest));
        self.by_text.insert(text.to_owned(), id);
        self.by_id.insert(id, Term { functor: String::new(), args: vec![], types: vec![], text: Some(text.to_owned()), split });
        id
    }

    pub fn text(&self, id: Cell) -> Option<&str> { self.by_id.get(&id)?.text.as_deref() }

    pub fn text_id(&self, text: &str) -> Option<Cell> { self.by_text.get(text).copied() }

    pub fn mint_any(&mut self, value: &AnyValue) -> Cell {
        if let AnyValue::Real(bits) = value {
            if f64::from_bits(*bits).is_nan() { return self.mint_any(&AnyValue::Null); }
        }
        if let Some(id) = self.by_any.get(value) { return *id; }
        if let AnyValue::Real(bits) = value {
            let n = f64::from_bits(*bits);
            if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 && (n as i64) as f64 == n {
                self.mint_any(&AnyValue::Integer(n as i64));
            }
        }
        let id = self.next.checked_add(1).expect("cell ID space exhausted");
        self.next = id;
        self.by_any.insert(value.clone(), id);
        self.any_by_id.insert(id, value.clone());
        id
    }

    pub fn any_value(&self, id: Cell) -> Option<&AnyValue> { self.any_by_id.get(&id) }

    pub fn any_key(&self, id: Cell) -> Option<Cell> {
        let AnyValue::Real(bits) = self.any_value(id)? else { return Some(id); };
        let n = f64::from_bits(*bits);
        if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 && (n as i64) as f64 == n {
            self.by_any.get(&AnyValue::Integer(n as i64)).copied()
        } else { Some(id) }
    }

    pub fn split(&self, id: Cell) -> Option<(Cell, Cell)> { self.by_id.get(&id)?.split }

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
        (Some(x), Some(y)) if x.text.is_some() || y.text.is_some() => match (x.text, y.text) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            _ => unreachable!(),
        },
        (Some(x), Some(y)) => x.args.len().cmp(&y.args.len())
            .then_with(|| x.functor.cmp(&y.functor))
            .then_with(|| {
                for ((a, ta), (b, tb)) in x.args.iter().zip(&x.types).zip(y.args.iter().zip(&y.types)) {
                    let order = match (ta, tb) {
                        (Ty::Id | Ty::Text, Ty::Id | Ty::Text) => compare(*a, *b, resolve),
                        (Ty::Real, Ty::Real) => f64::from_bits(*a as u64).partial_cmp(&f64::from_bits(*b as u64)).unwrap_or(Ordering::Equal),
                        _ => (*ta as u8).cmp(&(*tb as u8)).then_with(|| a.cmp(b)),
                    };
                    if order != Ordering::Equal { return order; }
                }
                Ordering::Equal
            }),
    }
}

fn key_number(out: &mut Vec<u8>, value: Cell) {
    out.extend_from_slice(&((value as u64) ^ (1u64 << 63)).to_be_bytes());
}

/// Key of an id that is not a dictionary term.
pub fn atom_key(id: Cell) -> Vec<u8> {
    let mut out = vec![0];
    key_number(&mut out, id);
    out
}

/// Key of one term, given the key of each `Ty::Id`/`Ty::Text` argument. Mint calls this once per term
/// with the stored keys of its children; a term never changes, so the result is final.
pub fn term_key(term: &Term, child_key: &mut impl FnMut(Cell) -> Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(text) = &term.text {
        out.push(1);
        for b in text.bytes() { if b == 0 { out.extend_from_slice(&[0, 255]); } else { out.push(b); } }
        out.extend_from_slice(&[0, 0]);
        return out;
    }
    out.push(2);
    out.extend_from_slice(&(term.args.len() as u32).to_be_bytes());
    for b in term.functor.bytes() {
        if b == 0 { out.extend_from_slice(&[0, 255]); } else { out.push(b); }
    }
    out.extend_from_slice(&[0, 0]);
    for (&arg, ty) in term.args.iter().zip(&term.types) {
        match ty {
            Ty::Int => { out.push(0); key_number(&mut out, arg); }
            Ty::Id => { out.push(1); out.extend(child_key(arg)); }
            Ty::Text => { out.push(2); out.extend(child_key(arg)); }
            Ty::Real => {
                out.push(3);
                let bits = if f64::from_bits(arg as u64) == 0.0 { 0 } else { arg as u64 };
                let ordered = if bits >> 63 == 0 { bits ^ (1 << 63) } else { !bits };
                out.extend_from_slice(&ordered.to_be_bytes());
            }
            Ty::Any => { out.push(4); key_number(&mut out, arg); }
        }
    }
    out
}

/// A byte key whose lexical order matches `compare` for acyclic terms. Recomputes from `resolve`;
/// the SQLite engine stores `term_key` at mint instead.
pub fn sort_key(id: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>) -> Vec<u8> {
    fn key(id: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>, depth: usize) -> Vec<u8> {
        if depth >= 128 { return atom_key(id); }
        let Some(term) = resolve(id) else { return atom_key(id); };
        term_key(&term, &mut |child| key(child, resolve, depth + 1))
    }
    key(id, resolve, 0)
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

    #[test]
    fn strings_share_ids_with_constructors_and_split_without_writes() {
        let mut terms = Interner::default();
        let compound = terms.mint("token", &[7], &[Ty::Int]);
        let later = terms.mint_text("écho");
        let earlier = terms.mint_text("ada");
        assert_eq!(terms.mint_text("écho"), later);
        assert_ne!(compound, later);
        assert_eq!(terms.compare(earlier, later), Ordering::Less);
        let count = terms.by_id.len();
        let (head, rest) = terms.split(later).unwrap();
        assert_eq!((terms.text(head), terms.text(rest)), (Some("é"), Some("cho")));
        assert_eq!(terms.by_id.len(), count);
        assert!(terms.split(terms.text_id("").unwrap()).is_none());
    }
}
