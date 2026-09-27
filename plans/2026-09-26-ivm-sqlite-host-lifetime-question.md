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

Decision, delegated to the coordinator on 2026-09-26: pass `&mut impl Host`
to both `Engine::settle` and `Engine::snapshot`. `Host::conn(&mut self)` exposes
the caller-owned or extension connection through an engine-neutral `Any`
reference. `Dd` ignores the host. `ivm-sqlite` resolves the connection for each
call and retains no borrowed pointer or owned connection.

The decision affects the SQLite `Engine` implementation, the shared runtime,
and the promoted two-engine lab harness. It does not change the committed
collector path or the existing SQLite tests.
