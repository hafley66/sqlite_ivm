use crate::{AnyValue, Cell, Row, Ty};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// `functor` and `text` are shared: the interner's maps and every copy of a
/// term point at one allocation per distinct string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub functor: Arc<str>,
    pub args: Row,
    pub types: Vec<Ty>,
    pub text: Option<Arc<str>>,
}

#[derive(Default)]
pub struct Interner {
    functors: HashSet<Arc<str>>,
    by_key: HashMap<(Arc<str>, Row), Cell>,
    by_id: BTreeMap<Cell, Term>,
    by_text: HashMap<Arc<str>, Cell>,
    by_any: BTreeMap<AnyValue, Cell>,
    any_by_id: BTreeMap<Cell, AnyValue>,
    next: Cell,
    pending: Vec<(Arc<str>, Row)>,
}

impl Interner {
    pub fn len(&self) -> usize { self.by_id.len() + self.any_by_id.len() }

    pub fn mint(&mut self, functor: &str, args: &[Cell], types: &[Ty]) -> Cell {
        let functor = match self.functors.get(functor) {
            Some(shared) => shared.clone(),
            None => {
                let shared: Arc<str> = Arc::from(functor);
                self.functors.insert(shared.clone());
                shared
            }
        };
        let key = (functor.clone(), args.to_vec());
        if let Some(id) = self.by_key.get(&key) {
            return *id;
        }
        let id = self.next.checked_add(1).expect("term ID space exhausted");
        self.next = id;
        self.by_key.insert(key, id);
        self.by_id.insert(id, Term { functor: functor.clone(), args: args.to_vec(), types: types.to_vec(), text: None });
        let mut row = vec![id];
        row.extend_from_slice(args);
        self.pending.push((functor, row));
        id
    }

    /// Interns `text` whole. Decompose mints the head and rest of a string when it reads it.
    pub fn mint_text(&mut self, text: &str) -> Cell {
        if let Some(id) = self.by_text.get(text) { return *id; }
        let id = self.next.checked_add(1).expect("term ID space exhausted");
        self.next = id;
        let shared: Arc<str> = Arc::from(text);
        self.by_text.insert(shared.clone(), id);
        self.by_id.insert(id, Term { functor: Arc::from(""), args: vec![], types: vec![], text: Some(shared) });
        id
    }

    pub fn text(&self, id: Cell) -> Option<&str> { self.by_id.get(&id)?.text.as_deref() }

    pub fn text_id(&self, text: &str) -> Option<Cell> { self.by_text.get(text).copied() }

    /// NaN interns as Null. An integral Real also interns its Integer, which `any_key` reads.
    pub fn mint_any(&mut self, value: &AnyValue) -> Cell {
        let value = match value {
            AnyValue::Real(bits) if f64::from_bits(*bits).is_nan() => &AnyValue::Null,
            value => value,
        };
        if let Some(id) = self.by_any.get(value) { return *id; }
        if let AnyValue::Real(bits) = value {
            let n = f64::from_bits(*bits);
            if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 && (n as i64) as f64 == n {
                let integer = AnyValue::Integer(n as i64);
                if !self.by_any.contains_key(&integer) { self.insert_any(integer); }
            }
        }
        self.insert_any(value.clone())
    }

    fn insert_any(&mut self, value: AnyValue) -> Cell {
        let id = self.next.checked_add(1).expect("cell ID space exhausted");
        self.next = id;
        self.by_any.insert(value.clone(), id);
        self.any_by_id.insert(id, value);
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

    pub fn drain_pending(&mut self) -> Vec<(Arc<str>, Row)> {
        std::mem::take(&mut self.pending)
    }

    pub fn get(&self, id: Cell) -> Option<&Term> {
        self.by_id.get(&id)
    }

    pub fn snapshot(&self, functor: &str) -> Vec<(Row, i64)> {
        self.by_id.iter().filter(|(_, term)| &*term.functor == functor).map(|(id, term)| {
            let mut row = vec![*id];
            row.extend(&term.args);
            (row, 1)
        }).collect()
    }

    /// `snapshot` of each functor, in one pass over the dictionary.
    pub fn snapshots(&self, functors: &[&str]) -> Vec<Vec<(Row, i64)>> {
        let mut slots: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, functor) in functors.iter().enumerate() {
            slots.entry(functor).or_default().push(index);
        }
        let mut out = vec![Vec::new(); functors.len()];
        for (id, term) in &self.by_id {
            let Some(indices) = slots.get(&*term.functor) else { continue };
            let mut row = vec![*id];
            row.extend(&term.args);
            for index in indices {
                out[*index].push((row.clone(), 1));
            }
        }
        out
    }

    pub fn compare(&self, a: Cell, b: Cell) -> Ordering {
        compare(a, b, &mut |id| self.get(id).cloned())
    }
}

/// Non-dictionary IDs are atomic symbols ordered by their integer value. Walks both terms with an
/// explicit stack in argument order; the first unequal comparison decides.
pub fn compare(a: Cell, b: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>) -> Ordering {
    enum Step { Terms(Cell, Cell), Done(Ordering) }
    let mut stack = vec![Step::Terms(a, b)];
    while let Some(step) = stack.pop() {
        let (a, b) = match step {
            Step::Done(Ordering::Equal) => continue,
            Step::Done(order) => return order,
            Step::Terms(a, b) => (a, b),
        };
        if a == b { continue; }
        let (x, y) = match (resolve(a), resolve(b)) {
            (None, None) => { stack.push(Step::Done(a.cmp(&b))); continue; }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (x, y),
        };
        if x.text.is_some() || y.text.is_some() {
            stack.push(Step::Done(match (&x.text, &y.text) {
                (Some(a), Some(b)) => a.cmp(b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                _ => unreachable!(),
            }));
            continue;
        }
        let head = x.args.len().cmp(&y.args.len()).then_with(|| x.functor.cmp(&y.functor));
        if head != Ordering::Equal { return head; }
        let pairs = x.args.iter().zip(&x.types).zip(y.args.iter().zip(&y.types));
        let steps: Vec<Step> = pairs.map(|((a, ta), (b, tb))| match (ta, tb) {
            (Ty::Id | Ty::Text, Ty::Id | Ty::Text) => Step::Terms(*a, *b),
            (Ty::Real, Ty::Real) => Step::Done(f64::from_bits(*a as u64).partial_cmp(&f64::from_bits(*b as u64)).unwrap_or(Ordering::Equal)),
            _ => Step::Done((*ta as u8).cmp(&(*tb as u8)).then_with(|| a.cmp(b))),
        }).collect();
        stack.extend(steps.into_iter().rev());
    }
    Ordering::Equal
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

/// Nesting depth past which `sort_key` keys a child as an atom.
const SORT_KEY_DEPTH: usize = 128;

/// A byte key whose lexical order matches `compare` for acyclic terms. Recomputes from `resolve`
/// with an explicit stack; the SQLite engine stores `term_key` at mint instead.
pub fn sort_key(id: Cell, resolve: &mut impl FnMut(Cell) -> Option<Term>) -> Vec<u8> {
    enum Step { Enter(Cell, usize), Exit(Term, usize) }
    let mut stack = vec![Step::Enter(id, 0)];
    let mut keys: Vec<Vec<u8>> = Vec::new();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(id, depth) => {
                let term = if depth >= SORT_KEY_DEPTH { None } else { resolve(id) };
                let Some(term) = term else { keys.push(atom_key(id)); continue; };
                let children: Vec<Cell> = term.args.iter().zip(&term.types)
                    .filter(|(_, ty)| matches!(ty, Ty::Id | Ty::Text)).map(|(arg, _)| *arg).collect();
                stack.push(Step::Exit(term, children.len()));
                stack.extend(children.into_iter().rev().map(|child| Step::Enter(child, depth + 1)));
            }
            Step::Exit(term, children) => {
                let mut child_keys = keys.split_off(keys.len() - children).into_iter();
                let key = term_key(&term, &mut |_| child_keys.next().unwrap_or_default());
                keys.push(key);
            }
        }
    }
    keys.pop().unwrap_or_default()
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
    fn strings_share_ids_with_constructors_and_store_only_the_whole_text() {
        let mut terms = Interner::default();
        let compound = terms.mint("token", &[7], &[Ty::Int]);
        let later = terms.mint_text("écho");
        let earlier = terms.mint_text("ada");
        assert_eq!(terms.mint_text("écho"), later);
        assert_ne!(compound, later);
        assert_eq!(terms.compare(earlier, later), Ordering::Less);
        assert_eq!((terms.text_id("é"), terms.text_id("cho"), terms.by_id.len()), (None, None, 3));
    }

    /// Named budget: bytes of the minted text.
    const MEGABYTE: usize = 1 << 20;
    /// Named budget: stack of the minting thread, far below what per-character recursion needs.
    const SMALL_STACK: usize = 64 << 10;

    #[test]
    fn megabyte_text_mints_on_a_small_stack_as_one_entry() {
        let text: String = "ab😀".repeat(MEGABYTE / 6);
        let (count, back) = std::thread::Builder::new().stack_size(SMALL_STACK).spawn(move || {
            let mut terms = Interner::default();
            let id = terms.mint_text(&text);
            (terms.by_id.len(), terms.text(id) == Some(text.as_str()))
        }).unwrap().join().unwrap();
        assert_eq!((count, back), (1, true));
    }

    /// Named budget: nesting depth of the chain, past `SORT_KEY_DEPTH`.
    const CHAIN: usize = 10_000;

    #[test]
    fn deep_terms_compare_and_key_on_a_small_stack() {
        let out = std::thread::Builder::new().stack_size(SMALL_STACK).spawn(|| {
            let mut terms = Interner::default();
            let (mut a, mut b) = (terms.mint_text("a"), terms.mint_text("b"));
            for _ in 0..CHAIN {
                a = terms.mint("wrap", &[a, 0], &[Ty::Id, Ty::Int]);
                b = terms.mint("wrap", &[b, 0], &[Ty::Id, Ty::Int]);
            }
            let low = terms.mint("wrap", &[a, 0], &[Ty::Id, Ty::Int]);
            let high = terms.mint("wrap", &[a, 1], &[Ty::Id, Ty::Int]);
            let mut resolve = |id| terms.get(id).cloned();
            let (low_key, high_key) = (sort_key(low, &mut resolve), sort_key(high, &mut resolve));
            (terms.compare(a, b), terms.compare(b, a), terms.compare(low, high), low_key < high_key)
        }).unwrap().join().unwrap();
        assert_eq!(out, (Ordering::Less, Ordering::Greater, Ordering::Less, true));
    }
}
