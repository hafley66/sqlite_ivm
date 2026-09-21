# pass 3 shapes: every test that reaches the old engine

Probe: `tests/zz_probe.rs` (one-off, deleted before the PR). "Engine today"
is read from `__ivm_schema.generic` after installing the test's shape through
the same entry the test uses (`sqlite_ivm_create` for vtab tests, a direct
`query::bind` call for binder unit tests). "Plan accepts" is a real
`relational::bind` call on the same shape, fresh database.

Decision rule from the brief: proceed only if every shape the old path serves
is accepted by `relational::bind`.

## tests/0_query.rs (unit tests over `query::bind`)

| test | shape | engine today | plan accepts |
|---|---|---|---|
| accepted_queries_bind_catalog_names_and_output_order | `g, COUNT(*), SUM(int)` single table; quoted/bracketed identifiers; `"odd"" table"` | direct-bind (old) | yes |
| binding_is_read_only_and_matches_sqlite_results_through_crud | `g, COUNT(*), SUM(int)` single table, CRUD rebind | direct-bind (old) | yes |
| unsupported_queries_are_rejected_without_changing_schema_or_rows | 36 rejected forms: HAVING, ORDER/LIMIT, compounds, WITH, DISTINCT, views, attached schemas, reserved names, TEXT/generated factors, AVG, expressions, windows, multi-group, no group, `GROUP BY 1` | direct-bind (old) | old-accepted subset: yes; plan additionally accepts HAVING, LIKE, text factor, generated column, AVG, no-group, multi-group, expression filters (superset, not served by old) |
| temporary_shadowing_is_rejected | source shadowed by a TEMP table | direct-bind (old) | no ("temporary source shadow") |
| unsupported_filter_forms_are_rejected_before_mutation | 17 rejected filters: float/string/NULL/IN/function/param/subquery/BETWEEN/bare literals, hex/real/underscore int forms | direct-bind (old) | plan accepts float/string/NULL/function forms (superset) |
| composite_join_binding_preserves_pairs_and_rejects_other_predicates | INNER JOIN, 1-3 equality `col=col` keys, parenthesized forms; rejects OR, inequalities, literals | direct-bind (old) | yes (all three accepted key forms) |
| recursive_shapes_bind_to_one_fixpoint_node_or_name_their_rejection | `WITH RECURSIVE` fixpoints, 2-4 rules | relational (direct `relational::bind`) | yes |
| comma_joins_take_equality_keys_from_where | comma joins, equality keys from WHERE | relational (direct `relational::bind`) | yes |

## tests/1_maintenance.rs (installed through the vtab)

| test | shape | engine today | plan accepts |
|---|---|---|---|
| managed_drop_preserves_sources_and_other_views_and_rolls_back | `items JOIN dimensions`, `COUNT(*)+SUM`, WHERE `i.amount>=7` | old | yes |
| failed_drop_restores_objects_metadata_and_caller_transaction | same join | old | yes |
| drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects | same join, odd view name `odd'"_% view` | old | yes |
| joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback | join grouped left (`i.group_id`) and right (`d.bucket`) | old | yes |
| deterministic_mutations_match_original_join_after_every_statement | 9 shapes: both group sides, AND/OR/NOT filters, column-vs-column filters, composite 2-3 keys, filtered composite | old | yes (all 9) |
| unrelated_groups_are_never_written_and_join_keys_are_indexed | join; observes `{name}_state(g,n,s)` writes and `__ivm_{name}_key_*` index plans | old | yes |
| rejected_values_overflow_and_writer_settings_preserve_state | join, NULL/text/real/overflow rejections | old | yes |
| install_failure_rolls_back_created_objects_and_preserves_caller_transaction | join, injected index/manifest failures | old | yes |
| single_table_integer_boundaries_and_public_view_are_enforced | single table `g, COUNT(*), SUM`, i64 bounds | old | yes |
| filters_cover_all_four_update_transitions_from_both_join_sides | join, WHERE `i.amount>0 AND d.factor<>0`, all four update transitions | old | yes |
| single_table_filters_boolean_precedence_integer_limits_and_rollback | 6 single-table filters: `>`, NOT-OR, OR-AND precedence, i64 bounds, swapped operands, `1=0` | old | yes (all 6) |
| columns_used_only_by_filters_are_checked_at_install_and_on_writes | join, filter-only columns `enabled` with bad stored values | old | yes |
| composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides | composite keys `(farm_id, join_key)`, both group sides, filtered variant | old | yes |

## Verdict

Every shape served by the old engine today (13 vtab shapes, all `old`) is
accepted by `relational::bind`: 13/13 yes. Every old-accepted `query::bind`
shape is accepted too. The relational plan is a strict superset for this
battery. Proceed to step 3: route every view through the relational plan.
