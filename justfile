# Correctness, native extension loading, and per-test timing budgets.
verify:
    bash scripts/9_verify.sh

# Native SQLite plugin, plain SQLite, and DD across twenty circuits.
shootout profile="smoke" *args:
    bash scripts/11_shootout.sh {{profile}} --engines sqlite-ivm,sqlite-query,dd {{args}}

# Historical rows/batch/fanout fixture, replayed through the current plugin.
crossover *args:
    bash scripts/12_crossover.sh {{args}}

# Five workloads: statement counts, row counts, mean/p99, and spans on/off wall.
every-statement:
    bash scripts/every-statement.sh

# Per-SQL VM-step attribution for one integration test executable.
statement-costs test="3_relational":
    bash scripts/statement-costs.sh {{test}}

# In-process size/fanout sweep with recompute oracles.
scale *args:
    CARGO_TARGET_DIR="$PWD/target" cargo run --offline --release --locked --manifest-path bench/Cargo.toml --bin bench -- scale {{args}}

package:
    bash scripts/10_package.sh

# Refresh source ages in the benchmark/lab table from the main checkouts.
benchmark-inventory *args:
    python3 scripts/13_benchmark_inventory.py {{args}}
