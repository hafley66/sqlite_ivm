# ir-mint-design (design doc only; no builds, no code edits)

Issue: `issuectl show ir-mint-op` (run in ~/projects/sprefa, read-only).
Goal: write plans/2026-09-26-ir-mint-design.md so the implementation lane can start the moment crates/ivm-* land.
The ivm crates are mid-promotion on branch feature/ivm-crate-promotion; read the current code there with `git show feature/ivm-crate-promotion:<path>` (crates/ivm-ir/src/0_ir.rs, crates/ivm-engine/src/1_rel.rs, crates/ivm-dd/src/2_dd.rs, crates/ivm-sqlite/src/{plan,catalog,4_engine}.rs).
sprefa term model: ~/projects/sprefa-wt/sqlite-perf-main/src/_6_eval/term.rs (Universe, TermId, intern), _1_program.rs, _4_kernel.rs (Order::TermLt, term_lt).

Doc sections, in this order (user's planning format):
1. Type signatures: Op::Mint shape in ivm-ir; the Rel trait method; the constructor/variant table RelKind; any Program fields. Pseudo-code comments for each body.
2. Instance timelines: who creates the interner and when (install / settle / reattach); its lifetime per engine (DD worker, SQLite catalog); what survives reattach.
3. Storage, sequence of reads and writes, and uniqueness: table/arrangement layout per engine; the insert-or-get sequence within one settle; the uniqueness condition (functor, args); the retraction semantics of minted ids (do ids ever die? what happens when every row referencing a ctor retracts?).
4. TermLt: compare (a) dictionary-backed ordering and (b) ids assigned in term order. Give a worked example with concrete ids for [a,b] vs [a,c], and each option's cost under insert in the middle of the order.
5. A worked example: the dl8 program `len([a,b])` lowered to IR with Mint and joins, as numbered nodes.
6. Test cases for the lab random harness: case / input / expected / why.
Every claim about existing code carries file:line. No recommendation beyond marking the open choices.
Commit subject: "plans: ir Mint design" Boop-Status done.

## Laws
- Work only in $PWD (your worktree). Never cd to a primary checkout. Never push. Never commit on main.
- No cargo builds.
- Final message receipt: status, sha, files, validation, next.
