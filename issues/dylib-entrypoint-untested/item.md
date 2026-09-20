---
created: 2026-09-19
updated: 2026-09-19
type: bug
status: open
priority: high
labels: [extension]
collision: [tests/**]
lane: lab-probe
lane_seq: 20
size: S
---

# No test loads the shipped extension through its entry point

## Description

Nothing in `cargo test` proves the shipped artifact loads.

Every test in `tests/*.rs` opens a connection and calls `register(&db)` directly,
which links the Rust in and skips `sqlite3_extension_init` at `src/3_extension.rs:84`
entirely. A renamed or broken init symbol ships silently.

The shell scripts (`scripts/1_crud.sh` and siblings) do load the real dylib, but they
are not part of `cargo test` and they need `sqlite3` on PATH.

## The gap

| tier | exercises the entry point | where |
|---|---|---|
| `register(&db)` | no | `tests/*.rs`, six files |
| `load_extension` from Rust | yes | `bench` feature exists, only examples use it |
| `sqlite3` CLI with `.load` | yes | `scripts/*.sh` |

The middle tier is missing and it is the cheap one. `rusqlite/load_extension` is
already an optional feature (`Cargo.toml:20`).

## Shape

One test: build with `--features extension`, open a connection, enable and call
`load_extension` on `target/debug/libsqlite_ivm.dylib`, run one `sqlite_ivm_create`,
assert the view answers.

## Acceptance Criteria

- [ ] a test loads the built dylib through `sqlite3_extension_init`
- [ ] it fails if the init symbol is renamed
- [ ] it picks the right extension per platform (dylib / so / dll)
- [ ] it skips with a clear message rather than failing when the artifact is unbuilt

## Test Plan

**What breaks if wrong:** the shipped `.dylib` stops loading and no test notices.
A renamed `sqlite3_extension_init`, a dropped `#[no_mangle]`, or a build that
silently omits the `extension` feature all ship green today.

**Seam:** `dlopen` plus `sqlite3_extension_init` (`src/3_extension.rs:84`). Everything
below that seam is already covered by `tests/0_query.rs` and siblings.

```rust
use oh::test;

#[test(timeout = "3s")]
fn extension_loads_through_its_entry_point() { ... }

#[test(timeout = "3s")]
fn loaded_extension_registers_the_create_function() { ... }

#[test(timeout = "5s")]
fn loaded_extension_maintains_a_view_end_to_end() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| loads | the built artifact for this platform | `load_extension` returns Ok | the only thing that proves `sqlite3_extension_init` is reachable |
| registers | a loaded connection | `SELECT sqlite_ivm_create(...)` resolves | init returning Ok while registering nothing is a real failure mode |
| maintains | one source table, one insert | the view answers with the new row | proves the vtab module registered, not just the scalar function |
| artifact absent | no dylib on disk | skips with a named message | `cargo test` without `scripts/0_build.sh` must not report a false failure |

**Untested and why:** the `sqlite3` CLI path stays in `scripts/*.sh`. Duplicating it
in Rust buys nothing and adds a PATH dependency to `cargo test`.

**Memory budget:** none. The dylib resolves its own allocator symbols, so the test
process's counting allocator cannot see plugin allocations. Stated so nobody adds a
budget here and trusts a number that means nothing.
