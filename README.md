# SQLite IVM

Transactional incremental SQL views for SQLite, implemented as a Rust loadable
extension. Ordinary source `INSERT`, `UPDATE`, and `DELETE` statements maintain
persistent results inside the source transaction. Rollback and savepoints cover
source rows, intermediate arrangements, and results together.

Version 0.3.0 is a prerelease with the explicitly bounded SQL contract below.
MIT OR Apache-2.0 licensed. The package is independent of the surrounding compiler.

## Use

```sql
.load ./libsqlite_ivm
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TABLE farms(id INTEGER PRIMARY KEY, region TEXT, price INTEGER);
CREATE VIRTUAL TABLE earnings USING sqlite_ivm('
  SELECT region, COUNT(*) AS farms, SUM(price) AS dollars, AVG(price) AS mean
  FROM farms GROUP BY region
');
INSERT INTO farms VALUES(1,'north',20),(2,'north',30);
SELECT * FROM earnings;                       -- north | 2 | 50 | 25.0
UPDATE farms SET price=40 WHERE id=1;
SELECT * FROM earnings;                       -- north | 2 | 70 | 35.0
DELETE FROM farms WHERE id=2;
SELECT * FROM earnings;                       -- north | 1 | 40 | 40.0
ALTER TABLE earnings RENAME TO income;
DROP TABLE income;
```

Load the extension on every reader and writer connection. Writers must enable
both pragmas. Public result CRUD is rejected. Read results with ordinary SELECT;
row order requires an outer ORDER BY. General results preserve bag multiplicity.
The specialized integer grouping path also indexes group-key equality reads.
The general virtual-table reader currently scans stored output rows.

## Supported query shapes

| Operator | Implemented contract |
|---|---|
| Projection and selection | Arithmetic and bit operations, comparisons, CASE, CAST, LIKE/GLOB, NULL tests, literal IN/BETWEEN, deterministic SQLite and registered scalar functions |
| Joins | INNER, LEFT, RIGHT, FULL, CROSS; ON, USING, NATURAL; comma joins and `JOIN` without ON take their equality keys from WHERE conjuncts; composite keys, residual predicates, non-equality conditions, self joins, parenthesized join trees |
| Existence | Correlated EXISTS / NOT EXISTS under AND, including filtered and joined subqueries |
| Aggregation | Composite groups and ordinals; COUNT, SUM, AVG, MIN, MAX; DISTINCT arguments, FILTER, aggregate expressions, HAVING, empty global aggregates |
| Sets | DISTINCT, UNION ALL, UNION, EXCEPT, INTERSECT |
| Top-k | Literal LIMIT/OFFSET, output aliases/ordinals, NULLS FIRST/LAST; composition with grouping, DISTINCT, compounds and windows |
| Windows | Multiple partitioned windows; ROW_NUMBER, RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST, NTILE, LAG/LEAD, FIRST/LAST/NTH_VALUE, aggregate windows, frames and named windows |
| Composition | FROM subqueries, CTEs with explicit column names, shared CTE consumers |
| Recursion | Positive `WITH RECURSIVE` strata: n-ary heads, several anchors, several recursive UNION distinct terms, inner and comma joins over sources, CTEs and subqueries, WHERE, DISTINCT, expression heads; consumers (aggregate, DISTINCT, EXISTS, NOT EXISTS, later recursive CTEs) are later strata; delete-and-rederive with semi-naive rowid-range rounds; cyclic insertion/deletion, NULL members and matching key collations |
| Values and collations | NULL, conforming INTEGER/REAL/TEXT/BLOB values; BINARY, NOCASE, RTRIM; adjacent IEEE floating values, infinities and empty BLOBs survive trigger transport |
| Transaction effects | Source triggers, generated columns, foreign-key cascades, savepoint rollback, writer contention, reader snapshots and maintenance failure rollback |

Acceptance includes the original 20 circuit families and 51 additional query
combinations, each with 174 source states. The native extension and independent
DD graphs are compared with a separate SQLite connection that has no extension.
Ordinary PostgreSQL queries provide another comparison. Exact results and the
pg_ivm 1.15 mismatch are recorded in [feature acceptance](bench/46_feature_acceptance.md).

Sources must be ordinary `main` tables. Values conform to declared affinities;
BLOB storage uses columns with BLOB or no declared affinity. Load deterministic
registered functions on every connection and keep their definitions and external
dependencies stable. Datetime functions and volatile functions are rejected.

The integer COUNT/SUM fast path is selected only when used columns are guaranteed
non-NULL and sources have no cascades or user triggers. Nullable queries use
relational arrangements. Accumulator overflow aborts the source statement.
Floating aggregation follows SQLite arithmetic; accumulation order can affect
rounding. ORDER BY ties retain SQL's unspecified tie order. Equality keys normalize
numeric equality; stored row identities preserve types and floating-point bits.

The accepted grammar has explicit boundaries. Aggregate/window composition and
expressions wrapping window calls require a FROM subquery. Window inheritance,
scalar subqueries, IN subqueries, aggregate EXISTS, EXISTS under OR, custom
collations, custom aggregates, bind parameters and queries without FROM are
unsupported. Inside a recursive step, aggregation, EXISTS, NOT EXISTS or IN over
the step's own relation is rejected as `recursive step may not aggregate or
negate its own relation`; `UNION ALL` recursion is rejected as `recursive UNION
ALL unsupported`; outer joins, USING, ORDER BY and LIMIT inside the recursive
CTE are rejected. SQLite 3.53 itself rejects mutual recursion between two CTEs
(`circular reference`) and a second reference to the recursive table in one
step (`multiple references to recursive table`), so "mutual" recursion is
spelled as one CTE with a discriminator column, for example `parity(node,odd)`.
A recursive CTE may consume an earlier recursive CTE; a fixpoint nested inside
another step is not spellable in SQLite and therefore not built. DD comparison uses signed row deltas and
sequential u64 epochs; the SQLite API exposes transactions and relational bags.
It does not expose arbitrary DD timestamps, frontiers, negative source bags,
custom Rust operators or durable DD execution.

## Source DDL

Use the managed source DDL functions so defining queries and ownership metadata
change in the same transaction:

```sql
SELECT sqlite_ivm_rename_source('farms','growers');
SELECT sqlite_ivm_rename_column('growers','price','cost');
SELECT sqlite_ivm_drop_source('growers',0); -- rejects managed dependents
SELECT sqlite_ivm_drop_source('growers',1); -- explicitly drops dependents too
```

Rename uses SQLite's own ALTER TABLE rewriting, regenerates source hooks, and
preserves result and intermediate rows. Public result column names stay fixed.
The functions participate in caller transactions and roll back atomically.
Direct source ALTER/DROP, ADD/DROP COLUMN, and source type changes are outside
this DDL protocol. Result CREATE/ALTER RENAME/DROP use ordinary SQLite DDL.
Compatibility functions `sqlite_ivm_create(name,query)` and `sqlite_ivm_drop(name)`
remain available.

## State and incremental work

`xCreate` creates persistent operator arrangements, indexes, source triggers, and
an ownership manifest, then loads existing source rows once. `xConnect` reads a
persisted result schema. Maintenance lazily binds the catalog query and refreshes
it after managed source DDL; reconnect does not populate results again.

Source hooks send OLD/NEW row images to `xUpdate`. Projection and filtering emit
signed rows. Joins look up matching keys. Sets maintain support counts. Aggregates
and windows emit the difference within the affected group. Top-k reads an indexed
candidate prefix. A recursive CTE keeps one member table per fixpoint; insertion
runs semi-naive rounds whose delta is a rowid range of that table, deletion
over-deletes the derivable region into a work table, rederives from the remaining
facts, then emits the net difference. Every round is one statement per recursive
term, so statement counts follow the changed rows and rounds, never the member
size. No mutation rebuilds the entire view from its defining SELECT. Large
affected groups or graph regions can still require large work.

`xRename` preserves state and index B-trees; `xDestroy` validates the exact owned
DDL before cleanup. Shared `__ivm_*` catalogs remain after the last view is dropped.
Storage format 2 records the operator layouts and typed row identities; format 3
adds the recursive member layout and is written only for views with a recursive
CTE, so the 0.2.x extension still maintains non-recursive views created here.
Recursive views created by 0.2.x keep format 2 and are rejected at first
maintenance with `recursive views from storage format 2 must be dropped and
re-created`. Databases containing format 1 views require the matching 0.1.x
extension. Automatic format migration is unavailable. The original ordinary-view
prototypes are not migrated.

Enable `SQLITE_DBCONFIG_DEFENSIVE` to prevent direct shadow-table writes. Reserved
hidden columns (`__ivm_source`, `__ivm_adding`, `__ivm_row`) and catalogs are internal
maintenance interfaces, not a security boundary. Direct internal commands and
catalog edits are outside the supported API. No persistent triggers are installed
on shadow tables, which is required for reopening on SQLite 3.53.2.

Maintenance emits `tracing` spans through `hafley-observe`, installed once on the
first `register` (extension load included) unless the host process already owns a
global subscriber. The default filter is `warn`, so nothing prints until
`RUST_LOG=sqlite_ivm=debug` (or another `RUST_LOG` filter) asks for it;
`HAFLEY_LOG_FORMAT=json` switches the format. Spans are `maintain` (view, source
table, sign), `fixpoint` (view, node, rows in, rounds) and `round` (phase, index,
rows written). Fields carry names and integers, never row contents.

## The engine as one reactive pipe

SQLite callbacks are the only producers. One subscribe, at the bottom. Every
operator below names the SQL statement it becomes; nothing per row runs in Rust
once a batch exists. The "today" column in the table after the code names the
per-row path this pipe replaces.

```ts
const sqlite$ = fromSqliteCallbacks(db)   // xBegin | xUpdate | xSavepoint | xRelease | xRollbackTo | xSync | xCommit | xRollback

const view$ = sqlite$.pipe(

  // transaction boundary: everything between xBegin and xSync|xRollback is one window
  windowToggle(
    sqlite$.pipe(filter(e => e.kind === 'xBegin')),
    () => sqlite$.pipe(filter(e => e.kind === 'xSync' || e.kind === 'xRollback')),
  ),

  mergeMap(transaction$ => transaction$.pipe(

    // collector: per-row events become one ordered batch, savepoints truncate
    scan((staged, e) => match(e, {
      xUpdate:     ({ table, sign, values }) => [...staged, { table, sign, values, sequence: staged.length }],
      xSavepoint:  ({ id }) => tag(staged, { mark: [id, staged.length] }),
      xRelease:    ({ id }) => untag(staged, id),
      xRollbackTo: ({ id }) => staged.slice(0, markOf(staged, id) ?? 0),   // absent mark = predates first write = 0
      default:     () => staged,
    }), [] as RowChange[]),

    // spill: over either ceiling the tail lives in <name>_delta, same sequence numbers
    map(staged => staged.length > STAGED_ROWS || bytes(staged) > STAGED_BYTES
      ? spillToDelta(db, staged) : staged),

    // drain point: deferred = last, eager = every xRelease, read = first xFilter, manual = UDF
    takeLast(1),                                    // mode 'deferred'
    // the other modes are bufferWhen(() => drainSignal$) with drainSignal$ one of the four above

    // one batch, one source_delta table; from here on everything is SQL over sets
    map(batch => writeDelta(db, batch)),            // INSERT INTO source_delta SELECT ... ; rows: |batch|

    // plan walk, topological; each node emits its delta given the input delta and the pre-state arrangements
    expand(delta => nodesFedBy(delta.node)),        // fan out to every consumer node, bounded by plan depth
    concatMap(({ node, delta }) => match(node.kind, {

      Map: ({ expressions, predicate }) =>
        of(delta).pipe(
          filter(row => predicate ? evalSql(predicate, row) : true),   // INSERT INTO node_delta SELECT exprs FROM in_delta WHERE predicate
          map(row => project(expressions, row)),
        ),

      Join: ({ left, right, mode }) =>
        of(delta).pipe(
          groupBy(row => row.side),
          mergeMap(side$ => side$.pipe(
            // delta(A join B) = deltaA join B_old, then update A, then A_new join deltaB
            concatMap(deltaA => concat(
              probeOther(db, node, deltaA),          // INSERT INTO join_delta SELECT d.*, b.*, d.__n*b.__n FROM a_delta d JOIN op_b b USING(__k)
              upsertArrangement(db, node, deltaA),   // UPDATE t SET __n=__n+d.__n FROM delta d WHERE t.__r=d.__r AND identity(t)=identity(d);
                                                     // INSERT ... WHERE NOT EXISTS; DELETE WHERE __n=0
            )),
          )),
          // outer modes: a second statement per side for the rows whose match count crossed zero
          filter(row => mode === 'inner' || crossedZero(row)),
        ),

      Set: ({ op }) =>
        of(delta).pipe(
          map(row => withKey(row, intern(db, row))),                    // dictionary id, one RETURNING seek
          scan((counts, row) => bump(counts, row.__k, row.side, row.__n), {}),
          map(counts => presentBefore(counts) !== presentAfter(counts, op) ? emitOne(counts) : nothing),
        ),

      Group: ({ keys, expressions, having, window, limit }) =>
        of(delta).pipe(
          map(row => project(keptColumns(node), row)),                   // the projection Map the planner already inserted
          groupBy(row => row.__k),                                        // touched keys only
          mergeMap(key$ => key$.pipe(
            withLatestFrom(readOld(db, node, key$.key)),                  // SELECT ... FROM _state WHERE __k = key
            concatMap(([rows, old]) => concat(
              upsertArrangement(db, node, rows),
              recompute(db, node, key$.key),                              // SELECT exprs FROM op_group WHERE __k=? [GROUP BY __k HAVING ...], or the window/limit CTE
            )),
            map(([old, fresh]) => difference(old, fresh)),               // retract old, add fresh
          )),
        ),

      Fixpoint: ({ rules }) =>
        of(delta).pipe(
          concatMap(rows => rows.some(r => r.__n < 0)
            ? deleteAndRederive(db, node, rows)                          // affected set, then rounds
            : of(rows)),
          expand(round => semiNaive(db, node, round), FIXPOINT_ROUNDS),   // next delta = rules(delta join arrangements) minus member; bounded, named
          takeWhile(round => round.length > 0, true),
          reduce((all, round) => all.concat(round), []),
        ),
    })),

    // output: every delta that reaches the output node lands in _state
    filter(({ node }) => node === plan.output),
    map(({ delta }) => applyState(db, delta)),                          // DELETE __key IN retracted; INSERT added

    // commit: the pager commits after xSync returns; xCommit asserts staged and *_delta are empty
    tap(() => truncateDeltas(db)),
  )),

  // failure anywhere aborts the statement; SQLite's journal unwinds every table the batch touched
  catchError(err => { throw sqliteError(err) }),
)

view$.subscribe()   // the one subscribe: SQLite's xSync is the boundary
```

One transaction on the marble, deferred mode. Source `orders` gets three rows
and one savepoint is rolled back:

```
xBegin  xUpdate xUpdate xSavepoint xUpdate xRollbackTo xUpdate                 xSync            xCommit
  |        a       b        mark       c       unwind       d                    |                 |
staged:   [a]    [a,b]    [a,b]+mark  [a,b,c]  [a,b]      [a,b,d]                |                 |
                                                                     drain ─────┤
                                                                     orders_delta <- a,b,d
                                                                     join   <- deltaA join B_old, upsert A
                                                                     group  <- keys{a,b,d}: old, upsert, recompute, diff
                                                                     _state <- retract, add
                                                                     truncate *_delta
                                                                                          assert empty
```

Where each operator lives:

| rxjs | today | after the batch port |
|---|---|---|
| `scan` collector | none; `insert()` runs SQL per row (`src/2_vtab.rs`) | the `sqlite-bulk-trigger` collector |
| `takeLast` and the drain modes | none | xSync, xRelease, xFilter, `sqlite_ivm_flush()` |
| Join `concatMap(concat(probe, upsert))` | `apply()` per row (`src/1a_relational.rs`) | two statements per side per transaction |
| Group `groupBy(__k)`, `withLatestFrom(old)`, `recompute`, `difference` | snapshot before, `change()`, snapshot after, per row | per touched key per transaction |
| Fixpoint `expand`, bounded | `fixpoint()`, already rounds | unchanged |
| `map(applyState)` | `emit()` per delta row | one DELETE, one INSERT |
| the one `subscribe` | trigger into the virtual table | xSync |

Statement sets per transaction of S statements over R rows on a node path of
depth D, by drain mode:

| mode | maintenance statement sets |
|---|---|
| per row (today) | R times D |
| eager | S times D |
| read | D per read that follows a write |
| deferred | D |

## Build, test, package

```bash
cargo fetch --locked --manifest-path sqlite_ivm/Cargo.toml
SQLITE3=sqlite3 bash sqlite_ivm/scripts/9_verify.sh
bash sqlite_ivm/scripts/10_package.sh
```

On macOS select a CLI with extension loading, for example Homebrew SQLite:

```bash
export SQLITE3="$(brew --prefix sqlite)/bin/sqlite3"
export SQLITE3_LIB_DIR="$(brew --prefix sqlite)/lib"
export SQLITE3_INCLUDE_DIR="$(brew --prefix sqlite)/include"
bash sqlite_ivm/scripts/9_verify.sh
```

The base gate runs Rust tests, six native Bash scenarios, and the original circuit
fixtures. The expanded feature gate and native value/transaction probes run with:

```bash
bash sqlite_ivm/scripts/12_features.sh /absolute/new/feature-artifacts
bash sqlite_ivm/scripts/14_native_values.sh /absolute/path/libsqlite_ivm.dylib
IVM_POSTGRES_PREFIX=/path/to/postgres \
  bash sqlite_ivm/scripts/13_feature_pg.sh /absolute/new/feature-artifacts
```

The raw PostgreSQL comparison returns failure on result mismatches. The separate
`15_pg_baseline.sh` gate checks the exact recorded pg_ivm 1.15 behavior, including
its FULL JOIN USING failure, while requiring every ordinary PG query to match.
The Linux/macOS workflow runs the expanded native/DD tests; a PostgreSQL job
checks the recorded 1.15 behavior. Remote CI execution has not been performed here.

The GitHub workflow builds and tests on Linux and macOS and uploads release
archives. Local execution is distinct from a remote CI run. Archives include the
native library, README, both licenses, and a SHA-256 sidecar. Source publication
or an external release requires selecting a destination.

## Shared benchmark

See [bench/README.md](bench/README.md) for reproducible commands, source provenance,
timing boundaries, receipts, and measured limits. The native SQLite consumer
loads the actual extension binary. PostgreSQL/pg_ivm and DD use the preserved
shared adapters, fixtures, and independent oracle. Unsupported pg_ivm families
are reported explicitly. SQLite and PostgreSQL are durable; DD is volatile.

## Reading order

1. `src/0_query.rs`: checked integer query binding.
2. `src/0a_catalog.rs`: ownership manifest and cleanup.
3. `src/0b_relational.rs`: general SELECT-to-operator compilation.
4. `src/1_maintenance.rs`: integer delta SQL and hooks.
5. `src/1a_relational.rs`: persistent relational operators.
6. `src/2_vtab.rs`: virtual-table lifecycle, scans, update dispatch.
7. `src/2a_source_ddl.rs`: transactional source DDL.
8. `src/3_extension.rs`: SQL functions and extension ABI.

rusqlite 0.40.2 and sqlite3-parser 0.17.0 are pinned in Cargo.lock. Rusqlite owns
the virtual-table adapters. A narrow ABI descriptor adapter adds xRename and
xShadowName through its public repr(transparent) Module layout; recheck that
layout before upgrading. No C source is required.

## Combined engine report

```bash
just ivm-shootout
# Or: bash sqlite_ivm/scripts/16_shootout.sh
```

Builds and runs semantic and performance comparisons across the native extension,
DD, Prolog, native PostgreSQL/pg_ivm, SQLite queries, and PGlite. The report includes
13 DD-specific operator/recursion/time contracts, 20 shared circuits, and 51 typed
query compositions. Unsupported definitions and result mismatches remain visible.
See [the command contract](bench/54_shootout.md) for dependencies, profiles,
artifacts and exit codes.
