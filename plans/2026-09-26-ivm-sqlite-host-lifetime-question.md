# SQLite Engine host lifetime

The selected state layout persists typed IR in `frontier_catalog` and compiles
it into the existing `Compiled` / `Root` / `SettleSql` path. SQL installs lower
to the same IR. Map/Filter, Threshold, Antijoin, and TopK have committed ports.

`Engine::install(program, host: &mut impl Host) -> Self` receives a borrowed
connection through `Host::connection()`. The trait's later `settle(&mut self,
frontier)` and `snapshot(&self, rel)` calls have no host argument. `Raw` borrows
the caller's `Connection`, and the extension's `Plugin` borrows its callback
connection. Neither borrow can be stored safely in the returned `Self` under
the current trait signature.

**Boop-Ask:** May `Engine::settle` and `Engine::snapshot` also receive a host
reference? The proposed signatures are `settle(&mut self, frontier: Frontier,
host: &mut impl Host)` and `snapshot(&self, rel: RelId, host: &impl Host)`. `Dd`
would ignore that argument; `ivm-sqlite` would resolve the connection for each
call. This keeps `Raw` caller-owned and lets `Plugin` use the extension's
connection without retaining an unsafe pointer. The alternative is a
lifetime-bound SQLite handle API outside the common `Engine` trait.

The answer affects the SQLite `Engine` implementation, the shared runtime,
and the promoted two-engine lab harness. It does not affect the committed
collector path or the existing SQLite tests.
