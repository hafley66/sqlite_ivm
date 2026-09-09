Charts from the September 9, 2026 full benchmark attempt.

The full run stopped with ENOSPC during recursive reachability. Only filter/map, join, and grouped-sum workloads completed every measurement at both input sizes. All eight implementations have 5 measured repetitions plus 1 warmup.

Recovered logs were independently checked against makeCircuitFixture: 48 timing cells, 3,120 mutation-state checks. The recovery script records reconstructed admission receipts explicitly. The overall full run remains incomplete.

Revalidate: node sqlite_ivm/bench/receipts/2_scaling_20260909/1_revalidate.mjs
Render PNGs (gnuplot required): node sqlite_ivm/bench/receipts/2_scaling_20260909/0_render.mjs

Input size is the initial row count in source a. Sources b and c initially contain 7 additional rows each. Mutations change row counts over time and include small graph fixtures after the bulk deletion. Lines join two measured points only.

changes_ms.png: median sum of 11 mutation states, excluding initial load and bulk deletion.
initial_ms.png: median initial-load time.
clear_ms.png: median deletion of all remaining rows in source a.

Each timing includes mutation, maintenance completion, and result read. SQLite IVM and PostgreSQL use durable commits; DD and Prolog are volatile; PGlite uses NodeFS. This is a local run with other machine activity.
