# Changelog

## Unreleased

- Replace plugin-generated JSON keys for set membership and source reads with indexed SQLite cells and integer dictionary identities. Storage format 11 rebuilds older views on writable reopen.
- Preserve exact row identity for bag accounting, including SQLite storage classes and real representations.
