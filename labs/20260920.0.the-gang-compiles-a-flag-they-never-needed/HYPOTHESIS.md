# the gang compiles a flag they never needed

## Hypothesis

SQLite's session extension, built into rusqlite on this toolchain, can observe the
writes this repo's tables actually receive and collapse them into one coalesced
changeset per maintenance step. If sessions cannot record tables with no declared
PRIMARY KEY, or cannot see writes that arrive through a virtual table's xUpdate,
no later arc can replace the generated trigger fleet with a session drain.

## Knob

Build flags. `SQLITE_ENABLE_SESSION` and `SQLITE_ENABLE_PREUPDATE_HOOK` in the
bundled amalgamation, reached through `rusqlite/session` plus `rusqlite/bundled`
(libsqlite3-sys 0.38.2 couples `session` to `preupdate_hook` at the feature level).
No engine code is touched.

## Invariants assumed of the input tables

- probes run on a fresh in-memory database in the main schema
- a probe writes a handful of rows, never more than the constants
  MAX_CHANGESET_ENTRIES and MAX_SCRIBED_ROWS protect
- the vtab probe's write path mirrors the repo's maintenance shape: one insert
  routed through xUpdate into a real table on the same connection

## Measurement

- `cargo build` and `cargo test` inside this directory: feature build and the five
  probe findings, printed per gate with `--nocapture`
- `cargo run`: the smallest program that attaches a session and drains a changeset
  (prints `drained 1 entries`, `SQLITE_INSERT`)
- flag isolation, against the libsqlite3-sys 0.38.2 amalgamation:
  - `cc -DSQLITE_ENABLE_SESSION -c sqlite3.c` then link a caller of
    `sqlite3session_create`: fails at link, `Undefined symbols: _sqlite3session_create`
  - same with `-DSQLITE_ENABLE_SESSION -DSQLITE_ENABLE_PREUPDATE_HOOK`:
    prints `sqlite3session_create rc=0`

## Invalidates

- Gate 2 returned no, so any arc that wants sessions must add declared PRIMARY KEYs
  to PK-less tables or exclude them; this repo has such tables, so a blanket
  "sessions replace the trigger fleet" claim is dead as stated.
- Gate 4b returned the expected split, so retraction arcs must use changesets;
  patchsets strip old non-PK values and cannot carry retraction.
- Gate 3 returned yes, so writes routed through a vtab xUpdate remain observable
  and the changeset arc keeps its input.

## Verdict

2026-09-20:

1. Does rusqlite build with the session feature on this toolchain, and does
   SQLITE_ENABLE_SESSION require SQLITE_ENABLE_PREUPDATE_HOOK?
   Yes, and yes. `cargo build` with `session` + `bundled` succeeds on
   aarch64-apple-darwin; libsqlite3-sys forces `preupdate_hook` (and
   `buildtime_bindgen`) whenever `session` is on; the amalgamation probe answers
   the flag question directly: session-only fails to link with
   `_sqlite3session_create` undefined, both flags print `rc=0`. The flags are a
   package deal, and through rusqlite you never face the choice.
2. Does a session record a table with no declared PRIMARY KEY? No. Probe:
   `CREATE TABLE nopk(note TEXT NOT NULL)`, session attached to all tables, one
   committed INSERT, changeset drains 0 entries. Probe:
   `session_records_a_table_without_a_declared_primary_key`.
3. Does a session see writes that arrived through a virtual table's xUpdate? Yes.
   Probe: `session_sees_writes_that_went_through_xupdate`, a writable vtab whose
   xUpdate inserts into `scribe_log(id INTEGER PRIMARY KEY, note TEXT)`: 1 row
   landed, 1 changeset entry recorded; the vtab write itself produced 0 entries.
   The later arc's input survives, but only because the real table has a declared
   key (gate 2).
4. Does a changeset coalesce three statements into one entry, and does a patchset
   strip old values while a changeset keeps them? Yes to both. Coalesce probe:
   insert, update, and an insert-delete pair drain to exactly 1 entry. Old-value
   probe: changeset carries `old(v) = Ok(Integer(1))`, patchset answers
   `old(v) = Err(InvalidColumnIndex(1))`.

System sqlite note: the SDK libsqlite3.tbd exports sqlite3session symbols and no
sqlite3_load_extension symbols; this lab runs bundled so the flag question stays
under test rather than the platform build.
