# batch at the boundary

## one drain per durable transaction is required

The `inserts_many_txn` scenario executes one hundred prepared `INSERT`
statements without an enclosing `BEGIN`. Each statement is an autocommit
transaction. Its drain is part of the same atomic unit as its source write.

The `fail_result` case in
`tests/5_transactions.rs::wal_snapshots_writer_contention_and_failed_maintenance_are_atomic`
installs a `BEFORE INSERT` trigger on `result_state`, then updates the source.
The injected maintenance failure rejects that source statement and leaves its
source row unchanged. Deferring maintenance across commits would report the
failure after the source row became durable, when that statement can no longer
roll back.

The 3,492 statements for one hundred autocommit inserts therefore describe one
hundred required atomic drains. The 225 statements for one hundred inserts in
one explicit transaction describe one drain carrying one hundred rows. The
remaining target is the fixed statement cost inside each drain, while retaining
one drain per transaction.

This follows the batching result from lab `lab-xsync` at `d2409ea`: source rows
inside one transaction reach maintenance as one batch. It also follows the
review finding that flushing at each trigger-driven statement savepoint loses
that batching because SQLite opens a statement savepoint around those writes.
Savepoints mark the collector instead.
