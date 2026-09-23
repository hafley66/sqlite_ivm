# Changelog

## Unreleased

- Skip initial map, inner-join, and set population when their required input rows are known empty. The create-group statement pin records two fewer materialization INSERTs and 20 fewer scanned rows; output hashes and correctness gates are unchanged. The six-run timing sample does not establish a performance gain.
- Replace plugin-generated JSON keys for set membership and source reads with indexed SQLite cells and integer dictionary identities. Storage format 11 rebuilds older views on writable reopen.
- Preserve exact row identity for bag accounting, including SQLite storage classes and real representations.
