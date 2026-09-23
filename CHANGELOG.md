# Changelog

## Unreleased

- Skip initial map, inner-join, and set population when their required input rows are known empty. The paired `2_partial` SQLite compile median fell from 4,615 to 4,490 ms; its output hash was unchanged.
- Replace plugin-generated JSON keys for set membership and source reads with indexed SQLite cells and integer dictionary identities. Storage format 11 rebuilds older views on writable reopen.
- Preserve exact row identity for bag accounting, including SQLite storage classes and real representations.
