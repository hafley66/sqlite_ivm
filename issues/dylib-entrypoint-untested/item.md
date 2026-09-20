---
created: 2026-09-19
updated: 2026-09-19
type: bug
status: open
priority: high
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
