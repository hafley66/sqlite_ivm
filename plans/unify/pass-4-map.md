# pass 4: split the compiler by clause

Pre-split line numbers refer to `src/0b_relational.rs` at `origin/main`
(commit `bdfe84e`, 2173 lines). The move is text-for-text; the only
non-move change is the `Field { .. }` constructor noted at the end.

## `src/0b_relational.rs`

Types, shared free helpers, node plumbing, and `bind`.

| item | lines | kind |
|---|---|---|
| `sql` | 5-13 | free helper |
| `Field` | 16-25 | type |
| `Kind` | 27-52 | type |
| `impl Kind` (`label`) | 53-68 | type impl |
| `Occurrence` | 70-73 | type |
| `Rule` | 77-83 | type |
| `impl Rule` (`member`, `mentions`) | 84-95 | type impl |
| `Node` | 97-101 | type |
| `Source` | 103-108 | type |
| `Plan` | 110-115 | type |
| `name` | 116-126 | free helper |
| `alias` | 127-131 | free helper |
| `resolve` | 132-153 | free helper |
| `explicit_collation` | 154-162 | free helper |
| `implicit_collation` | 163-174 | free helper |
| `collation` | 175-179 | free helper |
| `key_expression` | 180-190 | free helper (pub) |
| `key_sql` | 193-202 | free helper (pub) |
| `column_reference` | 203-209 | free helper (pub) |
| `expression` | 210-212 | free helper (pub) |
| `expression_aliases` | 213-457 | free helper |
| `affinity` | 518-538 | free helper |
| `expression_affinity` | 539-552 | free helper |
| `has_aggregate` | 553-600 | free helper |
| `ordinal` | 601-611 | free helper |
| `integer_limit` | 612-617 | free helper |
| `direction` | 618-624 | free helper |
| `nulls` | 625-631 | free helper |
| `Compiler` | 632-636 | type |
| `bind` | 2008-2107 | free helper (pub) |
| `field` | new | `Field` constructor |

## `src/0c_compile_from.rs`

| item | lines | kind |
|---|---|---|
| `table` | 710-834 | `impl Compiler` |
| `joined` | 835-886 | `impl Compiler` |
| `from` | 887-1065 | `impl Compiler` |
| `pairs` | 458-486 | free helper |
| `index_pairs` | 487-517 | free helper |
| `table_mentions` | 1810-1819 | free helper |
| `conjuncts` | 1821-1828 | free helper |
| `column_pair` | 1829-1845 | free helper |
| `from_mentions` | 1856-1862 | free helper |
| `part_mentions` | 1863-1870 | free helper |
| `select_mentions` | 1871-1878 | free helper |
| `part_expressions_mention` | 1879-1894 | free helper |
| `expr_mentions` | 1895-1926 | free helper |
| `equalities` | 1987-2007 | free helper |

## `src/0d_compile_select.rs`

| item | lines | kind |
|---|---|---|
| `push` | 638-647 | `impl Compiler` |
| `project_group_input` | 651-709 | `impl Compiler` |
| `predicate` | 1066-1121 | `impl Compiler` |
| `core` | 1122-1472 | `impl Compiler` |
| `select` | 1473-1577 | `impl Compiler` |

## `src/0e_compile_recursive.rs`

| item | lines | kind |
|---|---|---|
| `recursive` | 1578-1808 | `impl Compiler` |
| `where_keys_for_step` | 1849-1855 | free helper |
| `recursion_shape` | 1929-1986 | free helper |

## `src/0f_columns.rs`

| item | lines | kind |
|---|---|---|
| `visit_columns` | 2111-2153 | free helper |
| `column_references` | 2154-2162 | free helper |
| `renumber_columns` | 2163-2173 | free helper |

## Node plumbing placement

`push` and `project_group_input` are not named by the target table. `push`
is the shared node constructor; `project_group_input` projects the stored
columns of a `Group` (GROUP BY / window / LIMIT). Both live in
`0d_compile_select.rs` with the GROUP BY clause, keeping
`0b_relational.rs` (types, helpers, `bind`) at 696 lines.

## Non-move change

The `Field` literal is built at 791 (`table`), 1309 (`core` window),
1337 (`core` projection), 1403 (`core` ordering). All four call one
`pub(crate) fn field(qualifier, name, affinity, collation, visible,
unqualified) -> Field` in `0b_relational.rs`; `position` stays 0 and
`merged_star` stays false at every site.