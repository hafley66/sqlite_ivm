# Correctness, native extension loading, and per-test timing budgets.
verify:
    bash scripts/9_verify.sh

# Native SQLite plugin, plain SQLite, and DD across twenty circuits.
shootout profile="smoke" *args:
    bash scripts/11_shootout.sh {{profile}} --engines sqlite-ivm,sqlite-query,dd {{args}}

# Historical rows/batch/fanout fixture, replayed through the current plugin.
crossover *args:
    bash scripts/12_crossover.sh {{args}}

# Current plugin, historical counted C, DD, then hafley-observe diagnostic captures.
crossover-observe *args:
    python3 scripts/14_crossover_observe.py {{args}}

# Five workloads: statement counts, row counts, mean/p99, and spans on/off wall.
every-statement:
    bash scripts/every-statement.sh

# Per-SQL VM-step attribution for one integration test executable.
statement-costs test="3_relational":
    bash scripts/statement-costs.sh {{test}}

# In-process size/fanout sweep with recompute oracles.
scale *args:
    CARGO_TARGET_DIR="$PWD/target" cargo run --offline --release --locked --manifest-path bench/Cargo.toml --bin bench -- scale {{args}}

# Same access/aggregate frontier stream through ISO, extension, production, and DD.
frontier-stress *args:
    bash scripts/15_frontier_stress.sh {{args}}

package:
    bash scripts/10_package.sh

# Refresh source ages in the benchmark/lab table from the main checkouts.
benchmark-inventory *args:
    python3 scripts/13_benchmark_inventory.py {{args}}

# Pokémon trace demo: trace every oracle/pokemon script through DD and SQLite, then build one self-contained HTML page.
lab-20260924-dd-sqlite-pokemon-demo:
    #!/usr/bin/env bash
    set -euo pipefail
    cd labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow
    export CARGO_TARGET_DIR=../20260923.3.dd-inside-sqlite/target
    mkdir -p demo/traces
    for sql in oracle/pokemon/*.sql; do
        name=$(basename "$sql" .sql)
        cargo run --quiet --offline -j 2 --example 1_trace --features sqlite -- "pokemon/$name" "demo/traces/$name.json"
    done
    cd demo && pnpm install --frozen-lockfile && pnpm build
    echo "open $(pwd)/dist/index.html"
