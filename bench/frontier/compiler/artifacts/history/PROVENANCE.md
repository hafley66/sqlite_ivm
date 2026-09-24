# Historical captures — provenance

`56_2partial_138_rule_view.sql`

- Source of record: `plans/costs/56_2partial_138_rule_view.sql` in the MAIN
  sqlite_ivm checkout (outside this worktree). Copied verbatim on
  2026-09-24; sha256
  `8e71fe0760f028b4b2a95b6eb13b94631402012a7ec77bed3ab6235af6b6fe1a`;
  88,023 bytes; captured 2026-09-23 10:11 by an earlier lane.
- Shape: ONE whole-program `CREATE VIRTUAL TABLE "program" USING
  sqlite_ivm('WITH RECURSIVE ...')` covering the 138-rule `2_partial`
  program; store tables prefixed `main.n94677_a2`.
- Provenance distinction: this is the older whole-program lowering under a
  different compiler mode. The CURRENT lane's `dl8 emit sqlite` (sprefa
  `f468cbbe`) refuses every `2_partial` rule at emit (see the report, §0);
  the historical capture neither validates nor invalidates that failure,
  and the current emit failure does not invalidate the capture. Do not
  compare the two as if they were outputs of the same pipeline.

Related historical evidence, NOT copied here (see report §1):
`plans/costs/48_c2_standalone_replay.json` (c2_partial_3 case program,
78,219-byte view SQL under plugin `1d6d8d6`) and
`plans/costs/49_c2_fixpoint_native_keys.html`.
