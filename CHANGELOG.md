# Changelog

## Unreleased

- `Program::install` and `Program::install_unwatched` materialize the rows their sources already hold: one set-wise `INSERT .. SELECT` per source reads each source once, guards reject NULL or non-numeric group-sum storage explicitly, and a failed install rolls back with no catalog row, collector, or shadow objects. The installed snapshot equals a fresh evaluation of the defining SELECT; the frontier counter stays 0 and the initial delta stays empty.
- Skip initial map, inner-join, and set population when their required input rows are known empty. The create-group statement pin records two fewer materialization INSERTs and 20 fewer scanned rows; output hashes and correctness gates are unchanged. The six-run timing sample does not establish a performance gain.
- Replace plugin-generated JSON keys for set membership and source reads with indexed SQLite cells and integer dictionary identities. Storage format 11 rebuilds older views on writable reopen.
- Preserve exact row identity for bag accounting, including SQLite storage classes and real representations.
