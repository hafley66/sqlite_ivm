# sqlite_ivm recursion plan (temporary, deleted before PR)

## 0. SQLite grammar facts (probed 3.53.2, /tmp/probe.sql)

| shape | SQLite | consequence |
|---|---|---|
| two CTEs referencing each other | `circular reference: even` | mutual recursion unspellable; row 2 uses `parity(node,odd)` |
| two references to the recursive table in one step | `multiple references to recursive table` | one member occurrence per rule |
| NOT EXISTS / IN over own relation in step | `multiple recursive references` / `circular reference` | row 7 pre-check runs before prepare so the named text surfaces |
| aggregate in step | `recursive aggregate queries not supported` | same pre-check |
| several recursive SELECT terms (`anchor UNION s1 UNION s2`) | accepted | rules: Vec |
| two anchors, comma joins, DISTINCT, WHERE, two joins, subquery FROM | accepted | supported |
| earlier recursive CTE consumed by a later recursive CTE | accepted | sequential strata (row 6) |

## 1. Type signatures

```rust
// 0b_relational.rs
pub enum Occurrence { Input(usize) /* side */, Member }
pub struct Rule {
    pub occurrences: Vec<(Occurrence, usize /* width */)>,
    pub head: Vec<String>,          // rendered over the concatenated c{i} namespace
    pub predicate: Option<String>,  // ON conjunction AND WHERE, same namespace
    pub indexes: Vec<(Occurrence, String)>, // index expressions on local c{j}
}
Kind::Fixpoint { rules: Vec<Rule> }   // replaces Kind::Reach; node.inputs = one node per Input occurrence; node.fields = member fields
fn recursion_shape(select: &Select<'_>) -> Result<()>           // rows 7/8 named errors, before db.prepare
fn recursive(&mut self, cte: &CommonTableExpr<'_>) -> Result<usize>
fn key_sql(parts: &[(String, String)]) -> String                 // json_array of normalized key parts

// 1a_relational.rs
fn change(...) -> Result<(i64, i64)>                            // (old, new) multiplicity
fn fixpoint(&self, db, name, id, side, row: &Row, d: i64) -> Result<Vec<Delta>>
enum Role<'a> { Table(String), Params(&'a [Value]), Range(String, i64, i64) }
fn rule_body(rule: &Rule, roles: &[Role]) -> (String /* FROM ... WHERE ... */, Vec<Value>)
fn rounds(db, all: &str, rules: &[Rule], inputs: &[String], lo: i64) -> Result<()>   // semi-naive rowid-range rounds
```

## 2. Pseudo-code

```text
fixpoint(side,row,d):
  (old,new) = change(input side arrangement)
  appeared = old==0 && new>0 ; vanished = old>0 && new==0 ; else return []
  A = table(id, I), W = table(id, I+1)
  appeared:
    lo = max(rowid) of A
    for rule containing Input(side): INSERT OR IGNORE INTO A SELECT key,head FROM body(side:=Params(row), Member:=Table(A), others:=Table)
    rounds(A, lo)
    return rows of A with rowid>lo as +1
  vanished:
    DELETE FROM W
    for rule containing Input(side): INSERT OR IGNORE INTO W SELECT key,head FROM body(...) WHERE key IN A
    lo=0 loop: hi=max(rowid W); break if hi==lo
      DELETE FROM A WHERE __k IN (SELECT __k FROM W WHERE rowid in (lo,hi])
      for step rule: INSERT OR IGNORE INTO W SELECT key,head FROM body(Member:=Range(W,lo,hi), inputs:=Table) WHERE key IN A
      lo=hi
    lo_a = max(rowid A)
    INSERT OR IGNORE INTO A SELECT w.* FROM W w WHERE EXISTS(rule1 body(Member:=Table(A)) AND head IS w.c*) OR ...
    rounds(A, lo_a)
    out = SELECT c* FROM W w WHERE NOT EXISTS(A.__k=w.__k) as -1
    DELETE FROM W
rounds(A, lo): loop: hi=max(rowid A); break if hi==lo; for step rule: INSERT OR IGNORE INTO A SELECT key,head FROM body(Member:=Range(A,lo,hi), inputs:=Table); lo=hi
```

## 3. Instance lifetimes
- Rule strings: built at bind, live in Plan (rebuilt on refresh).
- Arrangement tables (inputs, A, W): created by create_state, dropped by xDestroy via manifest; rows live across transactions; rollback covers them.
- W: empty between maintenance calls (truncated at exit of the vanished path).

## 4. Storage layout
- input side s: `{name}_op{id}_{s}(__k TEXT NOT NULL,__r TEXT NOT NULL UNIQUE,__n INTEGER NOT NULL,c0..)` + `__k` index (generic) + expression indexes per equality column: `CREATE INDEX ON t(<rendered ref> COLLATE <cmp collation>)`.
- member all-table A: `{name}_op{id}_{I}(__k TEXT NOT NULL UNIQUE,c0..cw-1)` rowid table; expression indexes for member equality columns.
- work table W: `{name}_op{id}_{I+1}(__k TEXT NOT NULL UNIQUE,c0..cw-1)`.
- `__k` = `json_array(norm(key_expression(head_i, coll_i)) ...)`, norm: blob->json_object('blob',hex), integral real->integer, other real->json_object('real',printf('%!.17g')).

## 5. Read/write sequence per event
- insert: 1 change + 1 max(rowid) + R_side inserts + rounds (1 max + S inserts each) + 1 select of new rows.
- delete: 1 change + 1 truncate + R_side inserts + over-deletion rounds (1 max + 1 delete + S inserts each) + 1 max + 1 rederive insert + rounds + 1 diff select + 1 truncate.
- Statement count per event: O(rounds); rounds <= longest derivation chain. Per-row work only in emit() to consumers.

## 6. Uniqueness
- A.__k UNIQUE is the set semantics of UNION distinct (NULLs equal, 1 = 1.0, collation-normalized text).
- W.__k UNIQUE dedups over-deletion candidates.
- A rowid appended monotonic within a statement window; range (lo,hi] is the delta.
