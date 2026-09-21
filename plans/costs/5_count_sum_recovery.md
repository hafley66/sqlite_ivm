# COUNT/SUM recovery, 2026-09-21

The recovered performance target remains unmet. Same-machine medians for
12,000 initial rows, batch 1,000, fanout 200, three repetitions:

| Arm | Mutation + result materialization | Current / arm |
|---|---:|---:|
| Current sqlite_ivm plugin | 67.638 ms | 1.00× |
| Rebuilt historical counted C, sprefa e2052d5ae | 20.059 ms | 3.37× |
| Volatile DD | 1.000 ms | 67.64× |

All nine cases, three arms, three repetitions matched the five-state input and
output hashes. The quick circuit run produced 240 checksum-valid case-total
records across 20 circuits and three engines, including warmups, with no errors.
The root suite has 73 passing tests. Native loading has three passing tests;
CRUD and CLI checks pass. Existing growth classes and timing tolerances remain
unchanged. Exact SQL-cost pins were refreshed for the changed statements.

```bash
cd /Users/chrishafley/projects/sqlite_ivm && just crossover-observe
cd /Users/chrishafley/projects/sqlite_ivm && just verify
cd /Users/chrishafley/projects/sqlite_ivm && just shootout quick
```

## Implemented

- Arrangement UPSERT uses a unique index on hash plus exact row identity.
  Legacy hash-only indexes keep the previous exact-match path. Collision and
  format-5 reopen tests exercise both paths.
- A join feeding a group through plain column projections lets the group
  consolidate its signed input, removing the intermediate consolidation.
- Terminal groups with projected keys read affected stored before-images
  through an index. The existing damaged-result repair check remains covered.
- Terminal integer-key COUNT(*)/SUM groups maintain nullable support counts and
  eligibility in transactional shadow state. Signed small-integer contributions
  update stored sums; other value domains retain affected-group recomputation.
  Eligibility is conservative and stays invalid after a group leaves the domain.
- Format 6 prevents older binaries from silently leaving the new support state
  stale. Format 5 continues on its existing layout.

Tests cover nullable sums, multiple sums, last non-null deletion, empty globals,
large integers, REAL fallback, failed writes, savepoints, WAL snapshots, rename,
reopen, drop, hash collisions, and defensive shadow protection. A hafley-observe
growth assertion holds changed rows constant while growing the group 16×; the
COUNT/SUM drain remains in the constant VM-work class.

## Measurements and boundaries

Earlier paired starting measurements were 92.807 ms current / 25.150 ms
historical. Intermediate current medians were 82.095 ms after identity UPSERT,
74.331 ms after stored group before-images and join consolidation removal,
80.359 ms for a prototype that rescanned group members to establish eligibility,
and 67.158 ms after transactional eligibility/support counts. These separate
runs are retained under `bench/results/recovery-crossover-*`; host timing varies.

The final three-arm run is `bench/results/count-sum-observe/`. Its `timings/`
directory holds uninstrumented receipts. `current/` and `historical/` hold four
per-mutation Chrome traces, SQLite event stores, and profile.json. The compact
committed [receipt](4_count_sum_recovery.json) retains samples, hashes, and
diagnostic attribution. Full captures remain local because they include large
trace files and databases.

hafley-observe provides CountRecorder, event_stats, FieldStats, Chrome export,
dictionary-encoded SQLite event storage, and process sampling. SQL workloads,
oracles, phase attribution, and performance interpretations remain in sqlite_ivm.
Capture overhead is included only in diagnostic wall times. Nested SQLite
profile times overlap; raw counters accumulate on statement handles. Their sums
are not total execution work. Unavailable EXPLAIN plans retain their error.

Both SQLite timing arms use WAL/FULL. The historical arm uses indexed source
views and an 8 MiB page cache. The current arm uses persistent join/group
arrangements and default cache settings. Initial population differs and is
outside the mutation timer. DD is volatile.

The captured final database occupies 2,064,384 bytes for current SQLite and
1,159,168 bytes for the historical arm. Current traces show writes to both join
and group input arrangements. The historical source-view arm avoids those
duplicated row images. Removing that work while preserving general join and
aggregate fallback semantics remains outstanding.
