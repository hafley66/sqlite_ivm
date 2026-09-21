# Overnight herd log, 2026-09-20

Target (user): sqlite_ivm at acceptable perf for codebase querying through
sprefa-extract, then it carries dl8 eval's retraction and fixpoint in proper
relational form inside the plugin. Every row below is a receipt or a defect.

Rungs: dispatched, running, reported, verified (named check), merged.

## Board

| lane | preset | job | rung | pr |
|---|---|---|---|---|
| refactor/bench-rust | glm53f-omp-max | arc A: Rust bench binary, shootout + scale | running | |
| fix/open-bugs | sol-med | 6 items: 14_scale ignore, window LIMIT, growth assert, subtype ledger, TEXT keys, hygiene | running | |
| chore/archive-labs | sonnet subagent | labs, probes, crud scripts to archive/; lab issues closed | queued | |
| refactor/bench-rust arc B | glm53f-omp | delete 55 bench files | queued on arc A | |
| perf/scale | flash-omp-max | scale sweep with RSS, disk, arrangement rows, amplification; chain profile; fixes | queued on arc A + bugs | |
| refactor pass 1..4 | glm53f-omp | see passes | queued on bugs | |

## Passes (filled after the module map lands)

## Entries

- 03:5x merged #20, #21, #22; main `30a6e9c`.
- primary checkout `~/projects/sqlite_ivm` is stale at `39c6ac2` with older
  untracked copies of issue and docs files; the classifier blocked moving
  them. Lanes run from `origin/main`; nothing depends on it. Morning: move
  the strays aside, `git pull --ff-only`.
- dispatched refactor/bench-rust (m-eecc57e1), fix/open-bugs (m-9ba06a98).
