# Lab protocol

## Minting

```bash
./labs/new-lab.sh the-gang-copies-the-fts5-homework
```

The script computes the datestamp and index. Never hand-name a directory.

Title card rules, enforced by the script: starts `the-gang-`, lowercase kebab,
at least five words, at most 72 characters. The title card is the hypothesis.

## ISO

A lab has its own `[workspace]` and no path dependency on the entry point.
Shared crates are pinned to the exact versions the entry point resolves; the
script copies those lines out of the root `Cargo.toml`. A lab proves a
mechanism, not an integration.

## Test contract (user decision, 2026-09-19)

Testing is observation, so it lives in `hafley-observe`. No new harness crate.

| rule | mechanism |
|---|---|
| every test declares a timeout | `#[observe::test(timeout = "2s")]`, no default, omitting it is a compile error |
| no timeout over 10s | the macro parses the literal and rejects it at compile time |
| a bare `#[test]` cannot exist | `harness = false` plus `#![deny(dead_code)]` makes it a compile error |
| a test that overruns dies | `cargo nextest` `slow-timeout = { terminate-after = 1 }`, process-per-test |
| logs survive the kill | SIGTERM first, `grace-period` wide enough for the ring buffer to flush |
| seeded tests record their seed | span field, and the rerun command in the TAP YAML block |
| a failing seed is kept | written to a corpus directory, replayed first on every later run |

Wire format is TAP 14. The YAML block carries seed, elapsed, timeout, and the
literal rerun command. JUnit XML for CI comes from nextest directly.
