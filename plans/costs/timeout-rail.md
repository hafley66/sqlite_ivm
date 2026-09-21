# timeout rail: a clock on the gate

Base `origin/main` at `68a88321`. Worktree `chore/timeout-rail`. The gate is
`scripts/9_verify.sh` (no `justfile` exists in this repo, so `scripts/` is the
gate). Nothing in `src/` changes.

The incident this rail answers: a change made a consumer battery 1.7x slower
and one cell 2.7x slower, and every gate stayed green. The gates measured
correctness only.

## R1: build vs buy, candidate by candidate

Priced before a line of rail code. `cargo nextest 0.9.140` is already on the
machine and in no way assumed: the run below proves what it does.

| candidate | what it gives | what it costs | verdict |
|---|---|---|---|
| `cargo nextest` | per-test `slow-timeout` (`period`, `terminate-after`, `grace-period`), slow-test warnings, JUnit per-test durations, retries, one process per test so an overrunning test is killed and attributed | one binary to install in CI (`taiki-e/install-action`), one config file | **CHOSEN** for level 1 and as the timing source for level 3 |
| `#[timeout]` attributes (`ntest`, `test-with`) | a budget literal next to each test | edits to all 68 tests; a proc-macro dependency that changes `Cargo.lock`, which the gate runs `--locked`; no per-gate output; no process kill | REFUSED |
| `timeout(1)` around the cargo invocation | whole-gate wall ceiling, no per-test attribution | one wrapper line | ADOPTED, but only as level 2, which is precisely what it provides |
| GitHub Actions `timeout-minutes` | job-level ceiling, no per-test attribution | one workflow key | ADOPTED as the CI outer bound |
| hand-rolled runner, timer, or scheduler | the thing the law says to avoid | unbounded | REFUSED |

Level 1 is bought, not built. Level 2 is bought. Level 3 has no buyer: nextest
reports each test's wall and compares nothing to a recorded number. The gap is
filled by a comparator over nextest's own JUnit file. It times nothing, runs
nothing, and schedules nothing; it reads numbers nextest already wrote.

The proof that nextest enforces a per-test budget, names the test, and kills
the process is in R4.