# statement program, pass 2b

`Program` of SQL text built once per (view, connection) beside the `Plan` in
`Table`; `drain` takes `&Program`; `format!` on the drain path 64 -> 1. Change
taken uncommitted from `refactor/pass-2-statement-program`, landed here as
18f8971, measured, opened as a structural PR.

`HAFLEY_LOG=sqlite=debug cargo test --release --test 8_group_limit --
--nocapture 2>&1 | grep -o 'time.busy=[0-9.]*[a-zµ]*'`, summed per run in
seconds. Three runs per side, sequential, separate target dirs.

| side | run 1 s | run 2 s | run 3 s |
|---|---|---|---|
| before, origin/main a35146c | 10.821 | 10.631 | 10.708 |
| after, 18f8971 | 10.612 | 10.693 | 10.622 |

After max 10.693 is above before min 10.631: overlap. The pass-2 land rule
(all three after below all three before) fails. 121425 spans per run on both
sides, identical statement count; the removed text building hides inside the
noise of sqlite=debug logging at this test size.

Probe with logging off, same worktrees, sequential: `cargo test --release
--test 8_group_limit`, wall from the `finished in` line.

| side | run 1 s | run 2 s | run 3 s |
|---|---|---|---|
| before, origin/main a35146c | 0.93 | 0.95 | 0.94 |
| after, 18f8971 | 0.76 | 0.75 | 0.75 |

All three after at or below the before max 0.95: the "not slower" rule holds.
PR opened as a structural refactor, no gain claimed.

## other receipts
- battery: `cargo test` three runs, 17 binaries, 75 passed each, none new,
  none changed.
- `tests/13_statements_per_drain.rs` green, counts unchanged. It emits zero
  `time.busy` spans on both sides (it counts prepares through its own hook),
  so the refactor-2 leg "same for 13_statements_per_drain" has no numbers to
  sum on either side.
- `grep -c 'format!' src/1d_drain.rs`: 64 before, 1 after. The remaining site
  (src/1d_drain.rs:158) formats the missing-multiplicity error from row
  counts; failure branch only, never on the success path.
- `git diff --stat origin/main...HEAD`: src/1c_materialize.rs,
  src/1d_drain.rs, src/1e_program.rs (new), src/2_vtab.rs, src/lib.rs.
  Owned files only.
- `cargo clippy -- -D warnings`: clean.
