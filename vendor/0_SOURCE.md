# Vendored hafley-rs crates

| Crate | Upstream revision |
|---|---|
| `hafley-observe` | [1fbf655a](https://github.com/hafley66/hafley-rs/tree/1fbf655a0ed229c43a0eb59caa87206025059dc9/crates/hafley-observe) |
| `sqlite-bulk-trigger` | [29ce8a5e](https://github.com/hafley66/hafley-rs/tree/29ce8a5e454643e845dc98dcc278ad55bd61e024/crates/sqlite-bulk-trigger) |

Each directory contains the tracked crate files and both upstream licenses.
Cargo workspace metadata and dependencies are expanded in each manifest so
checkout does not require an external worktree. Shared library changes belong
upstream, then are copied here. Application query compilation and maintenance SQL
remain in sqlite_ivm.
