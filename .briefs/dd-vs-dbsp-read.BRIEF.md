# dd-vs-dbsp-read (research; doc only; no builds)

Goal: write plans/2026-09-26-dd-vs-dbsp.md comparing differential-dataflow 0.25.1 (as used in crates/ivm-dd/src/2_dd.rs, or the lab src/2_dd.rs on main; DD source in ~/.cargo/registry/src/*/differential-dataflow-0.25.1) with Feldera DBSP (~/projects/ext/feldera/crates/dbsp).
Read the actual code, not docs or cherry-picked examples.
Table rows: join (incremental bilinear form), recursion/fixpoint, aggregation (incl. min/max retraction), state storage (arrangement/trace vs spine), spill-to-disk, time model (lattice timestamps vs clock-cycle Z-sets), operator construction cost, API ergonomics (the lines to write one join + one recursive reach in each). Every cell carries file:line in both codebases.
Add a section mapping each crates/ivm-engine trait Rel method (get mfp union negate join antijoin reduce threshold topk letrec) to the DBSP operator that would implement it, with file:line.
Report data only; no recommendation paragraph.
Commit subject: "plans: DD vs DBSP code read" with Boop-Status done.

## Laws
- Work only in $PWD (your worktree). Never cd to a primary checkout. Never push. Never commit on main.
- No cargo builds.
- Final message receipt: status, sha, files, validation, next.
