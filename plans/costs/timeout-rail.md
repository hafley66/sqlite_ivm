# timeout rail: a clock on the gate

Base `origin/main` at `68a88321`. Worktree `chore/timeout-rail`. The gate is
`scripts/9_verify.sh` (no `justfile` exists in this repo, so `scripts/` is the
gate). Nothing in `src/` changes.

The incident this rail answers: a change made a consumer battery 1.7x slower
and one cell 2.7x slower, and every gate stayed green. The gates measured
correctness only.

## R1: build vs buy, candidate by candidate

Priced before a line of rail code. `cargo nextest 0.9.140` is already on the
machine and in no way assumed: the run below proves what it does.

| candidate | what it gives | what it costs | verdict |
|---|---|---|---|
| `cargo nextest` | per-test `slow-timeout` (`period`, `terminate-after`, `grace-period`), slow-test warnings, JUnit per-test durations, retries, one process per test so an overrunning test is killed and attributed | one binary to install in CI (`taiki-e/install-action`), one config file | **CHOSEN** for level 1 and as the timing source for level 3 |
| `#[timeout]` attributes (`ntest`, `test-with`) | a budget literal next to each test | edits to all 68 tests; a proc-macro dependency that changes `Cargo.lock`, which the gate runs `--locked`; no per-gate output; no process kill | REFUSED |
| `timeout(1)` around the cargo invocation | whole-gate wall ceiling, no per-test attribution | one wrapper line | ADOPTED, but only as level 2, which is precisely what it provides |
| GitHub Actions `timeout-minutes` | job-level ceiling, no per-test attribution | one workflow key | ADOPTED as the CI outer bound |
| hand-rolled runner, timer, or scheduler | the thing the law says to avoid | unbounded | REFUSED |
| the `hafley-observe` rails, already a path dependency | `assert_growth` names a span's complexity class between two input sizes; `StatementCounters` reads `vm_step` and `fullscan_step` per statement; `sqlite::instrument` turns both on with one call | nothing new. The crate is already a dependency, `src/3_extension.rs:54` already calls `instrument`, and `tests/10_growth.rs` and `tests/13_statements_per_drain.rs` already use it | **CHOSEN** for level 3. Deterministic, exact, no clock |

Level 1 is bought, not built. Level 2 is bought. Level 3 has no buyer in
nextest: it reports each test's wall and compares nothing to a recorded number.

Which levels are timed and which are deterministic:

| level | what it does | clock? |
|---|---|---|
| 1, per test | nextest `slow-timeout` period, `terminate-after = 1` | timed. A hang has no count to read |
| 2, per battery | `timeout(1)` ceiling, and `timeout-minutes` in CI | timed. A stall has no count to read |
| 3, per statement | pinned `vm_step`, `fullscan_step` and `events` per statement, exact | **deterministic.** One run, no tolerance, cannot flap |
| 3, per phase | pinned node span instances per kind, and the growth class of each phase span and of each view's `vm_step` | **deterministic.** One run, no tolerance, cannot flap |
| 3, residue | the recorded per-test wall and battery wall | timed, because a wall moves with page cache, I/O and lock waits while every count holds still |

The timed residue keeps a warmup discard, so a cold run cannot enter the
baseline, and the gate builds every test binary before the timed run, so a cold
run cannot enter the measurement either. R3 states both. The exact pins are the
legs that answer the incident: a statement that doubles or rises forty percent
moves a pinned number, and a busy machine moves none of them.

The proof that nextest enforces a per-test budget, names the test, and kills
the process is in R4, beside the proofs for the deterministic legs.

## R2: the base timings, three runs, raw

`cargo nextest run --no-fail-fast --manifest-path Cargo.toml`, warm build, at
`68a88321`, rail off (the profile carries no budget in this measurement). Three
runs. Machine `Darwin arm64`, 12 cores, load average 3.9 while other lanes run,
so the numbers are a little above a fully idle host. Raw output per run, only
the nextest section, cargo's two `unexpected cfg` warnings dropped:

Base state. No known-red record exists in this repo: `.github/CI-KNOWN-RED.md`
appears in old briefs under `archive/plans/` but in no branch (checked with
`git log --all -- .github/CI-KNOWN-RED.md` and `git ls-tree` over `main`,
`chore/observe-rails`, `plan/unify`, and `feature/every-statement`). So the
battery is re-measured rather than assumed: all three runs below are 68 passed,
0 skipped, wall 9.31s, 9.25s, 9.26s. A budget only terminates a slow leg; a
wrong answer still fails as a test failure first.

### run 1

```
$ cargo nextest run --no-fail-fast --manifest-path Cargo.toml
 Nextest run ID b46038fc-0d6a-442f-ad18-cc56d71aff6e with nextest profile: default
    Starting 68 tests across 14 binaries
        PASS [   0.008s] ( 1/68) sqlite-ivm::12_compass compass_passes_through_the_cli
        PASS [   0.010s] ( 2/68) sqlite-ivm::11_intern the_row_hash_is_the_published_fnv1a_64_vector
        PASS [   0.034s] ( 3/68) sqlite-ivm::11_intern dropping_the_view_drops_its_dictionary
        PASS [   0.034s] ( 4/68) sqlite-ivm::11_intern the_identity_splits_where_the_key_folds_and_joins_where_unique_would_split
        PASS [   0.035s] ( 5/68) sqlite-ivm::11_intern interning_is_injective_and_resolve_round_trips_the_corpus
        PASS [   0.035s] ( 6/68) sqlite-ivm::11_intern re_interning_adds_no_row_and_moves_no_id
        PASS [   0.038s] ( 7/68) sqlite-ivm::11_intern hashing_the_identity_keeps_one_arrangement_row_per_composite
        PASS [   0.043s] ( 8/68) sqlite-ivm::1_maintenance columns_used_only_by_filters_are_checked_at_install_and_on_writes
        PASS [   0.044s] ( 9/68) sqlite-ivm::10_growth group_limit_entries_stay_constant_in_input_multiplicity
        PASS [   0.018s] (10/68) sqlite-ivm::1_maintenance drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects
        PASS [   0.018s] (11/68) sqlite-ivm::1_maintenance install_failure_rolls_back_created_objects_and_preserves_caller_transaction
        PASS [   0.021s] (12/68) sqlite-ivm::1_maintenance filters_cover_all_four_update_transitions_from_both_join_sides
        PASS [   0.012s] (13/68) sqlite-ivm::1_maintenance rejected_values_overflow_and_writer_settings_preserve_state
        PASS [   0.022s] (14/68) sqlite-ivm::1_maintenance failed_drop_restores_objects_metadata_and_caller_transaction
        PASS [   0.022s] (15/68) sqlite-ivm::1_maintenance joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback
        PASS [   0.067s] (16/68) sqlite-ivm::12_compass compass_passes_in_process
        PASS [   0.016s] (17/68) sqlite-ivm::1_maintenance single_table_integer_boundaries_and_public_view_are_enforced
        PASS [   0.025s] (18/68) sqlite-ivm::1_maintenance managed_drop_preserves_sources_and_other_views_and_rolls_back
        PASS [   0.063s] (19/68) sqlite-ivm::1_maintenance composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides
        PASS [   0.027s] (20/68) sqlite-ivm::1_maintenance single_table_filters_boolean_precedence_integer_limits_and_rollback
        PASS [   0.022s] (21/68) sqlite-ivm::3_relational comma_joins_take_equality_keys_from_where
        PASS [   0.092s] (22/68) sqlite-ivm::10_growth drain_spans_stay_linear_in_changed_rows
        PASS [   0.017s] (23/68) sqlite-ivm::3_relational ignored_and_replaced_source_updates_preserve_arrangements
        PASS [   0.029s] (24/68) sqlite-ivm::2_vtab indexed_cursors_preserve_output_order_affinity_and_simultaneous_reads
        PASS [   0.040s] (25/68) sqlite-ivm::2_vtab defensive_shadow_protection_allows_source_dml_and_native_lifecycle
        PASS [   0.048s] (26/68) sqlite-ivm::2_vtab catalog_identity_survives_vacuum_and_rename_rejects_modified_hooks
        PASS [   0.048s] (27/68) sqlite-ivm::2_vtab ddl_rename_preserves_state_without_writes_and_drop_preserves_sources
        PASS [   0.010s] (28/68) sqlite-ivm::3_relational recursive_shapes_bind_to_one_fixpoint_node_or_name_their_rejection
        PASS [   0.017s] (29/68) sqlite-ivm::3_relational narrow_source_column_order_changes_are_transactional
        PASS [   0.019s] (30/68) sqlite-ivm::3_relational null_recursive_keys_weighted_overflow_and_aggregate_exists
        PASS [   0.015s] (31/68) sqlite-ivm::3_relational relational_group_locality_and_ddl_preserve_btrees
        PASS [   0.012s] (32/68) sqlite-ivm::3_relational unsupported_clause_combinations_fail_before_install
        PASS [   0.047s] (33/68) sqlite-ivm::3_relational global_empty_aggregates_and_source_ddl
        PASS [   0.024s] (34/68) sqlite-ivm::3_relational nullable_text_composite_groups_outer_join_and_rollback
        PASS [   0.065s] (35/68) sqlite-ivm::2_vtab ddl_rename_rollback_savepoints_and_failure_restore_usable_names
        PASS [   0.130s] (36/68) sqlite-ivm::13_statements_per_drain statements_per_drain_do_not_grow_with_the_batch
        PASS [   0.025s] (37/68) sqlite-ivm::4_features materialized_output_affinity_matches_ordinary_view_consumers
        PASS [   0.018s] (38/68) sqlite-ivm::4_features recursive_delete_preserves_alternate_null_support_and_collated_roots
        PASS [   0.038s] (39/68) sqlite-ivm::4_features comma_join_arrangements_hold_side_row_counts_never_the_product
        PASS [   0.033s] (40/68) sqlite-ivm::4_features parenthesized_join_scopes_and_nonrecursive_union_cte
        PASS [   0.058s] (41/68) sqlite-ivm::4_features blobs_and_adjacent_floats_survive_trigger_transport
        PASS [   0.036s] (42/68) sqlite-ivm::4_features row1_binary_closure_with_cycles
        PASS [   0.040s] (43/68) sqlite-ivm::4_features row2_parity_over_roots
        PASS [   0.084s] (44/68) sqlite-ivm::4_features collations_control_group_distinct_join_and_outer_predicates
        PASS [   0.049s] (45/68) sqlite-ivm::4_features row4_min_distance_aggregate_after_bounded_recursion
        PASS [   0.014s] (46/68) sqlite-ivm::4_features rows7_and_8_named_rejections_leave_no_state
        PASS [   0.091s] (47/68) sqlite-ivm::4_features comma_join_star_predicate_matches_plain_query
        PASS [   0.070s] (48/68) sqlite-ivm::4_features row3_filtered_step_with_two_joins_and_distinct
        PASS [   0.010s] (49/68) sqlite-ivm::6_extension_load extension_loads_through_its_entry_point
        PASS [   0.033s] (50/68) sqlite-ivm::5_transactions deterministic_registered_scalars_and_rejected_volatile_functions
        PASS [   0.008s] (51/68) sqlite-ivm::6_extension_load loaded_extension_maintains_a_view_end_to_end
        PASS [   0.075s] (52/68) sqlite-ivm::4_features row5_antijoin_and_exists_after_recursion
        PASS [   0.037s] (53/68) sqlite-ivm::5_transactions recursive_member_tables_follow_savepoints_and_rollback
        PASS [   0.008s] (54/68) sqlite-ivm::6_extension_load loaded_extension_registers_the_create_function
        PASS [   0.047s] (55/68) sqlite-ivm::5_transactions wal_snapshots_writer_contention_and_failed_maintenance_are_atomic
        PASS [   0.063s] (56/68) sqlite-ivm::5_transactions cascades_generated_values_and_user_trigger_writes_compose
        PASS [   0.092s] (57/68) sqlite-ivm::4_features row6_sequential_fixpoints_and_two_step_rules
        PASS [   0.035s] (58/68) sqlite-ivm::7_key_agreement json_subtype_group_key_agrees_between_bulk_and_incremental_paths
        PASS [   0.038s] (59/68) sqlite-ivm::7_key_agreement identity_round_trips_the_corpus
        PASS [   0.045s] (60/68) sqlite-ivm::7_key_agreement rust_key_after_expression_agrees_with_sql_key_sql
        PASS [   0.056s] (61/68) sqlite-ivm::9_fixpoint_retraction fixpoint_retraction_emits_the_stored_representative
        PASS [   0.251s] (62/68) sqlite-ivm::3_relational deterministic_relational_mutations_and_type_contract
        PASS [   0.219s] (63/68) sqlite-ivm::3_relational shared_circuit_states
        PASS [   0.118s] (64/68) sqlite-ivm::8_group_limit window_with_limit_reads_every_copy
        PASS [   0.455s] (65/68) sqlite-ivm::1_maintenance deterministic_mutations_match_original_join_after_every_statement
        PASS [   1.320s] (66/68) sqlite-ivm::4_features feature_compositions_against_sqlite
        PASS [   1.558s] (67/68) sqlite-ivm::8_group_limit group_limit_matches_plain_sql_across_multiplicity_limit_and_offset
        PASS [   8.938s] (68/68) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
────────────
     Summary [   9.061s] 68 tests run: 68 passed, 0 skipped
real 9.31
user 13.45
sys 1.06
```

### run 2

```
$ cargo nextest run --no-fail-fast --manifest-path Cargo.toml
 Nextest run ID c9b3fa53-810c-4b68-989b-167cfd6de1b2 with nextest profile: default
    Starting 68 tests across 14 binaries
        PASS [   0.009s] ( 1/68) sqlite-ivm::11_intern the_row_hash_is_the_published_fnv1a_64_vector
        PASS [   0.009s] ( 2/68) sqlite-ivm::11_intern the_identity_splits_where_the_key_folds_and_joins_where_unique_would_split
        PASS [   0.010s] ( 3/68) sqlite-ivm::11_intern dropping_the_view_drops_its_dictionary
        PASS [   0.009s] ( 4/68) sqlite-ivm::12_compass compass_passes_through_the_cli
        PASS [   0.013s] ( 5/68) sqlite-ivm::11_intern interning_is_injective_and_resolve_round_trips_the_corpus
        PASS [   0.015s] ( 6/68) sqlite-ivm::11_intern re_interning_adds_no_row_and_moves_no_id
        PASS [   0.016s] ( 7/68) sqlite-ivm::11_intern hashing_the_identity_keeps_one_arrangement_row_per_composite
        PASS [   0.016s] ( 8/68) sqlite-ivm::1_maintenance columns_used_only_by_filters_are_checked_at_install_and_on_writes
        PASS [   0.020s] ( 9/68) sqlite-ivm::10_growth group_limit_entries_stay_constant_in_input_multiplicity
        PASS [   0.017s] (10/68) sqlite-ivm::1_maintenance drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects
        PASS [   0.024s] (11/68) sqlite-ivm::1_maintenance failed_drop_restores_objects_metadata_and_caller_transaction
        PASS [   0.016s] (12/68) sqlite-ivm::1_maintenance rejected_values_overflow_and_writer_settings_preserve_state
        PASS [   0.024s] (13/68) sqlite-ivm::1_maintenance filters_cover_all_four_update_transitions_from_both_join_sides
        PASS [   0.021s] (14/68) sqlite-ivm::1_maintenance install_failure_rolls_back_created_objects_and_preserves_caller_transaction
        PASS [   0.025s] (15/68) sqlite-ivm::1_maintenance joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback
        PASS [   0.040s] (16/68) sqlite-ivm::12_compass compass_passes_in_process
        PASS [   0.015s] (17/68) sqlite-ivm::1_maintenance single_table_integer_boundaries_and_public_view_are_enforced
        PASS [   0.034s] (18/68) sqlite-ivm::1_maintenance managed_drop_preserves_sources_and_other_views_and_rolls_back
        PASS [   0.028s] (19/68) sqlite-ivm::1_maintenance single_table_filters_boolean_precedence_integer_limits_and_rollback
        PASS [   0.017s] (20/68) sqlite-ivm::2_vtab defensive_shadow_protection_allows_source_dml_and_native_lifecycle
        PASS [   0.049s] (21/68) sqlite-ivm::1_maintenance composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides
        PASS [   0.019s] (22/68) sqlite-ivm::2_vtab indexed_cursors_preserve_output_order_affinity_and_simultaneous_reads
        PASS [   0.013s] (23/68) sqlite-ivm::3_relational comma_joins_take_equality_keys_from_where
        PASS [   0.028s] (24/68) sqlite-ivm::2_vtab ddl_rename_preserves_state_without_writes_and_drop_preserves_sources
        PASS [   0.030s] (25/68) sqlite-ivm::2_vtab catalog_identity_survives_vacuum_and_rename_rejects_modified_hooks
        PASS [   0.068s] (26/68) sqlite-ivm::10_growth drain_spans_stay_linear_in_changed_rows
        PASS [   0.013s] (27/68) sqlite-ivm::3_relational ignored_and_replaced_source_updates_preserve_arrangements
        PASS [   0.011s] (28/68) sqlite-ivm::3_relational recursive_shapes_bind_to_one_fixpoint_node_or_name_their_rejection
        PASS [   0.019s] (29/68) sqlite-ivm::3_relational null_recursive_keys_weighted_overflow_and_aggregate_exists
        PASS [   0.020s] (30/68) sqlite-ivm::3_relational narrow_source_column_order_changes_are_transactional
        PASS [   0.041s] (31/68) sqlite-ivm::2_vtab ddl_rename_rollback_savepoints_and_failure_restore_usable_names
        PASS [   0.017s] (32/68) sqlite-ivm::3_relational nullable_text_composite_groups_outer_join_and_rollback
        PASS [   0.011s] (33/68) sqlite-ivm::3_relational unsupported_clause_combinations_fail_before_install
        PASS [   0.016s] (34/68) sqlite-ivm::3_relational relational_group_locality_and_ddl_preserve_btrees
        PASS [   0.083s] (35/68) sqlite-ivm::13_statements_per_drain statements_per_drain_do_not_grow_with_the_batch
        PASS [   0.036s] (36/68) sqlite-ivm::3_relational global_empty_aggregates_and_source_ddl
        PASS [   0.013s] (37/68) sqlite-ivm::4_features materialized_output_affinity_matches_ordinary_view_consumers
        PASS [   0.021s] (38/68) sqlite-ivm::4_features comma_join_arrangements_hold_side_row_counts_never_the_product
        PASS [   0.027s] (39/68) sqlite-ivm::4_features parenthesized_join_scopes_and_nonrecursive_union_cte
        PASS [   0.019s] (40/68) sqlite-ivm::4_features recursive_delete_preserves_alternate_null_support_and_collated_roots
        PASS [   0.037s] (41/68) sqlite-ivm::4_features blobs_and_adjacent_floats_survive_trigger_transport
        PASS [   0.030s] (42/68) sqlite-ivm::4_features row1_binary_closure_with_cycles
        PASS [   0.029s] (43/68) sqlite-ivm::4_features row2_parity_over_roots
        PASS [   0.008s] (44/68) sqlite-ivm::4_features rows7_and_8_named_rejections_leave_no_state
        PASS [   0.059s] (45/68) sqlite-ivm::4_features collations_control_group_distinct_join_and_outer_predicates
        PASS [   0.040s] (46/68) sqlite-ivm::4_features row4_min_distance_aggregate_after_bounded_recursion
        PASS [   0.016s] (47/68) sqlite-ivm::5_transactions deterministic_registered_scalars_and_rejected_volatile_functions
        PASS [   0.077s] (48/68) sqlite-ivm::4_features comma_join_star_predicate_matches_plain_query
        PASS [   0.027s] (49/68) sqlite-ivm::5_transactions cascades_generated_values_and_user_trigger_writes_compose
        PASS [   0.010s] (50/68) sqlite-ivm::6_extension_load extension_loads_through_its_entry_point
        PASS [   0.021s] (51/68) sqlite-ivm::5_transactions recursive_member_tables_follow_savepoints_and_rollback
        PASS [   0.010s] (52/68) sqlite-ivm::6_extension_load loaded_extension_maintains_a_view_end_to_end
        PASS [   0.064s] (53/68) sqlite-ivm::4_features row3_filtered_step_with_two_joins_and_distinct
        PASS [   0.008s] (54/68) sqlite-ivm::6_extension_load loaded_extension_registers_the_create_function
        PASS [   0.015s] (55/68) sqlite-ivm::7_key_agreement identity_round_trips_the_corpus
        PASS [   0.013s] (56/68) sqlite-ivm::7_key_agreement json_subtype_group_key_agrees_between_bulk_and_incremental_paths
        PASS [   0.035s] (57/68) sqlite-ivm::5_transactions wal_snapshots_writer_contention_and_failed_maintenance_are_atomic
        PASS [   0.028s] (58/68) sqlite-ivm::7_key_agreement rust_key_after_expression_agrees_with_sql_key_sql
        PASS [   0.091s] (59/68) sqlite-ivm::4_features row5_antijoin_and_exists_after_recursion
        PASS [   0.082s] (60/68) sqlite-ivm::4_features row6_sequential_fixpoints_and_two_step_rules
        PASS [   0.029s] (61/68) sqlite-ivm::9_fixpoint_retraction fixpoint_retraction_emits_the_stored_representative
        PASS [   0.239s] (62/68) sqlite-ivm::3_relational deterministic_relational_mutations_and_type_contract
        PASS [   0.103s] (63/68) sqlite-ivm::8_group_limit window_with_limit_reads_every_copy
        PASS [   0.248s] (64/68) sqlite-ivm::3_relational shared_circuit_states
        PASS [   0.415s] (65/68) sqlite-ivm::1_maintenance deterministic_mutations_match_original_join_after_every_statement
        PASS [   1.343s] (66/68) sqlite-ivm::4_features feature_compositions_against_sqlite
        PASS [   1.523s] (67/68) sqlite-ivm::8_group_limit group_limit_matches_plain_sql_across_multiplicity_limit_and_offset
        PASS [   8.914s] (68/68) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
────────────
     Summary [   9.000s] 68 tests run: 68 passed, 0 skipped
real 9.25
user 13.41
sys 0.93
```

### run 3

```
$ cargo nextest run --no-fail-fast --manifest-path Cargo.toml
 Nextest run ID 5d3e2460-0ab7-47c8-beb3-87f106d6ef31 with nextest profile: default
    Starting 68 tests across 14 binaries
        PASS [   0.008s] ( 1/68) sqlite-ivm::11_intern the_row_hash_is_the_published_fnv1a_64_vector
        PASS [   0.009s] ( 2/68) sqlite-ivm::12_compass compass_passes_through_the_cli
        PASS [   0.010s] ( 3/68) sqlite-ivm::11_intern interning_is_injective_and_resolve_round_trips_the_corpus
        PASS [   0.010s] ( 4/68) sqlite-ivm::11_intern the_identity_splits_where_the_key_folds_and_joins_where_unique_would_split
        PASS [   0.013s] ( 5/68) sqlite-ivm::11_intern hashing_the_identity_keeps_one_arrangement_row_per_composite
        PASS [   0.013s] ( 6/68) sqlite-ivm::11_intern dropping_the_view_drops_its_dictionary
        PASS [   0.015s] ( 7/68) sqlite-ivm::11_intern re_interning_adds_no_row_and_moves_no_id
        PASS [   0.016s] ( 8/68) sqlite-ivm::10_growth group_limit_entries_stay_constant_in_input_multiplicity
        PASS [   0.017s] ( 9/68) sqlite-ivm::1_maintenance columns_used_only_by_filters_are_checked_at_install_and_on_writes
        PASS [   0.013s] (10/68) sqlite-ivm::1_maintenance drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects
        PASS [   0.019s] (11/68) sqlite-ivm::1_maintenance failed_drop_restores_objects_metadata_and_caller_transaction
        PASS [   0.021s] (12/68) sqlite-ivm::1_maintenance install_failure_rolls_back_created_objects_and_preserves_caller_transaction
        PASS [   0.023s] (13/68) sqlite-ivm::1_maintenance filters_cover_all_four_update_transitions_from_both_join_sides
        PASS [   0.018s] (14/68) sqlite-ivm::1_maintenance rejected_values_overflow_and_writer_settings_preserve_state
        PASS [   0.037s] (15/68) sqlite-ivm::12_compass compass_passes_in_process
        PASS [   0.023s] (16/68) sqlite-ivm::1_maintenance managed_drop_preserves_sources_and_other_views_and_rolls_back
        PASS [   0.025s] (17/68) sqlite-ivm::1_maintenance joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback
        PASS [   0.023s] (18/68) sqlite-ivm::1_maintenance single_table_filters_boolean_precedence_integer_limits_and_rollback
        PASS [   0.018s] (19/68) sqlite-ivm::1_maintenance single_table_integer_boundaries_and_public_view_are_enforced
        PASS [   0.013s] (20/68) sqlite-ivm::3_relational comma_joins_take_equality_keys_from_where
        PASS [   0.021s] (21/68) sqlite-ivm::2_vtab catalog_identity_survives_vacuum_and_rename_rejects_modified_hooks
        PASS [   0.049s] (22/68) sqlite-ivm::1_maintenance composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides
        PASS [   0.023s] (23/68) sqlite-ivm::2_vtab defensive_shadow_protection_allows_source_dml_and_native_lifecycle
        PASS [   0.023s] (24/68) sqlite-ivm::2_vtab indexed_cursors_preserve_output_order_affinity_and_simultaneous_reads
        PASS [   0.010s] (25/68) sqlite-ivm::3_relational ignored_and_replaced_source_updates_preserve_arrangements
        PASS [   0.028s] (26/68) sqlite-ivm::2_vtab ddl_rename_preserves_state_without_writes_and_drop_preserves_sources
        PASS [   0.040s] (27/68) sqlite-ivm::2_vtab ddl_rename_rollback_savepoints_and_failure_restore_usable_names
        PASS [   0.013s] (28/68) sqlite-ivm::3_relational recursive_shapes_bind_to_one_fixpoint_node_or_name_their_rejection
        PASS [   0.019s] (29/68) sqlite-ivm::3_relational null_recursive_keys_weighted_overflow_and_aggregate_exists
        PASS [   0.024s] (30/68) sqlite-ivm::3_relational narrow_source_column_order_changes_are_transactional
        PASS [   0.080s] (31/68) sqlite-ivm::10_growth drain_spans_stay_linear_in_changed_rows
        PASS [   0.016s] (32/68) sqlite-ivm::3_relational relational_group_locality_and_ddl_preserve_btrees
        PASS [   0.010s] (33/68) sqlite-ivm::3_relational unsupported_clause_combinations_fail_before_install
        PASS [   0.027s] (34/68) sqlite-ivm::3_relational nullable_text_composite_groups_outer_join_and_rollback
        PASS [   0.041s] (35/68) sqlite-ivm::3_relational global_empty_aggregates_and_source_ddl
        PASS [   0.010s] (36/68) sqlite-ivm::4_features materialized_output_affinity_matches_ordinary_view_consumers
        PASS [   0.101s] (37/68) sqlite-ivm::13_statements_per_drain statements_per_drain_do_not_grow_with_the_batch
        PASS [   0.022s] (38/68) sqlite-ivm::4_features comma_join_arrangements_hold_side_row_counts_never_the_product
        PASS [   0.028s] (39/68) sqlite-ivm::4_features blobs_and_adjacent_floats_survive_trigger_transport
        PASS [   0.022s] (40/68) sqlite-ivm::4_features parenthesized_join_scopes_and_nonrecursive_union_cte
        PASS [   0.020s] (41/68) sqlite-ivm::4_features recursive_delete_preserves_alternate_null_support_and_collated_roots
        PASS [   0.033s] (42/68) sqlite-ivm::4_features row2_parity_over_roots
        PASS [   0.036s] (43/68) sqlite-ivm::4_features row1_binary_closure_with_cycles
        PASS [   0.012s] (44/68) sqlite-ivm::4_features rows7_and_8_named_rejections_leave_no_state
        PASS [   0.044s] (45/68) sqlite-ivm::4_features row4_min_distance_aggregate_after_bounded_recursion
        PASS [   0.085s] (46/68) sqlite-ivm::4_features collations_control_group_distinct_join_and_outer_predicates
        PASS [   0.011s] (47/68) sqlite-ivm::5_transactions deterministic_registered_scalars_and_rejected_volatile_functions
        PASS [   0.068s] (48/68) sqlite-ivm::4_features row3_filtered_step_with_two_joins_and_distinct
        PASS [   0.030s] (49/68) sqlite-ivm::5_transactions cascades_generated_values_and_user_trigger_writes_compose
        PASS [   0.018s] (50/68) sqlite-ivm::5_transactions recursive_member_tables_follow_savepoints_and_rollback
        PASS [   0.009s] (51/68) sqlite-ivm::6_extension_load extension_loads_through_its_entry_point
        PASS [   0.006s] (52/68) sqlite-ivm::6_extension_load loaded_extension_maintains_a_view_end_to_end
        PASS [   0.007s] (53/68) sqlite-ivm::6_extension_load loaded_extension_registers_the_create_function
        PASS [   0.074s] (54/68) sqlite-ivm::4_features row5_antijoin_and_exists_after_recursion
        PASS [   0.112s] (55/68) sqlite-ivm::4_features comma_join_star_predicate_matches_plain_query
        PASS [   0.008s] (56/68) sqlite-ivm::7_key_agreement json_subtype_group_key_agrees_between_bulk_and_incremental_paths
        PASS [   0.013s] (57/68) sqlite-ivm::7_key_agreement identity_round_trips_the_corpus
        PASS [   0.037s] (58/68) sqlite-ivm::5_transactions wal_snapshots_writer_contention_and_failed_maintenance_are_atomic
        PASS [   0.020s] (59/68) sqlite-ivm::7_key_agreement rust_key_after_expression_agrees_with_sql_key_sql
        PASS [   0.076s] (60/68) sqlite-ivm::4_features row6_sequential_fixpoints_and_two_step_rules
        PASS [   0.031s] (61/68) sqlite-ivm::9_fixpoint_retraction fixpoint_retraction_emits_the_stored_representative
        PASS [   0.214s] (62/68) sqlite-ivm::3_relational deterministic_relational_mutations_and_type_contract
        PASS [   0.102s] (63/68) sqlite-ivm::8_group_limit window_with_limit_reads_every_copy
        PASS [   0.236s] (64/68) sqlite-ivm::3_relational shared_circuit_states
        PASS [   0.426s] (65/68) sqlite-ivm::1_maintenance deterministic_mutations_match_original_join_after_every_statement
        PASS [   1.326s] (66/68) sqlite-ivm::4_features feature_compositions_against_sqlite
        PASS [   1.526s] (67/68) sqlite-ivm::8_group_limit group_limit_matches_plain_sql_across_multiplicity_limit_and_offset
        PASS [   8.928s] (68/68) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
────────────
     Summary [   9.016s] 68 tests run: 68 passed, 0 skipped
real 9.26
user 13.43
sys 0.88
```

Wall of the whole battery, nextest real time per run: 9.31s, 9.25s, 9.26s. Sum
of the per-test walls per run, which is not the wall because nextest runs tests
across binaries in parallel: 15.189s, 14.446s, 14.459s.

## R3: the budget for every test

Rule: budget = max(1s, median x 3). The 1s floor covers the 64 legs at or below
1s; four legs measured above 1s carry their own derived budget. The floor is not
a guess: the largest absolute run-to-run spread seen is 0.047s (on a 0.101s
leg), so 1s is 21x the worst jitter observed, and 100x the fastest leg observed
(0.006s). The multiplier is 3 on the slowest leg whose spread is 0.27 percent,
which leaves 300 percent of headroom over anything observed.

| test | run 1 | run 2 | run 3 | median | budget s | arithmetic |
|---|---|---|---|---|---|---|
| `sqlite-ivm::10_growth::drain_spans_stay_linear_in_changed_rows` | 0.092 | 0.068 | 0.080 | 0.080 | 1 | floor, median x3 = 0.24 |
| `sqlite-ivm::10_growth::group_limit_entries_stay_constant_in_input_multiplicity` | 0.044 | 0.020 | 0.016 | 0.020 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::11_intern::dropping_the_view_drops_its_dictionary` | 0.034 | 0.010 | 0.013 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::11_intern::hashing_the_identity_keeps_one_arrangement_row_per_composite` | 0.038 | 0.016 | 0.013 | 0.016 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::11_intern::interning_is_injective_and_resolve_round_trips_the_corpus` | 0.035 | 0.013 | 0.010 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::11_intern::re_interning_adds_no_row_and_moves_no_id` | 0.035 | 0.015 | 0.015 | 0.015 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::11_intern::the_identity_splits_where_the_key_folds_and_joins_where_unique_would_split` | 0.034 | 0.009 | 0.010 | 0.010 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::11_intern::the_row_hash_is_the_published_fnv1a_64_vector` | 0.010 | 0.009 | 0.008 | 0.009 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::12_compass::compass_passes_in_process` | 0.067 | 0.040 | 0.037 | 0.040 | 1 | floor, median x3 = 0.12 |
| `sqlite-ivm::12_compass::compass_passes_through_the_cli` | 0.008 | 0.009 | 0.009 | 0.009 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::13_statements_per_drain::statements_per_drain_do_not_grow_with_the_batch` | 0.130 | 0.083 | 0.101 | 0.101 | 1 | floor, median x3 = 0.30 |
| `sqlite-ivm::1_maintenance::columns_used_only_by_filters_are_checked_at_install_and_on_writes` | 0.043 | 0.016 | 0.017 | 0.017 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::1_maintenance::composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides` | 0.063 | 0.049 | 0.049 | 0.049 | 1 | floor, median x3 = 0.15 |
| `sqlite-ivm::1_maintenance::deterministic_mutations_match_original_join_after_every_statement` | 0.455 | 0.415 | 0.426 | 0.426 | 1.3 | median x3 = 1.3, rounded to 1.3 |
| `sqlite-ivm::1_maintenance::drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects` | 0.018 | 0.017 | 0.013 | 0.017 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::1_maintenance::failed_drop_restores_objects_metadata_and_caller_transaction` | 0.022 | 0.024 | 0.019 | 0.022 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::1_maintenance::filters_cover_all_four_update_transitions_from_both_join_sides` | 0.021 | 0.024 | 0.023 | 0.023 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::1_maintenance::install_failure_rolls_back_created_objects_and_preserves_caller_transaction` | 0.018 | 0.021 | 0.021 | 0.021 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::1_maintenance::joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback` | 0.022 | 0.025 | 0.025 | 0.025 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::1_maintenance::managed_drop_preserves_sources_and_other_views_and_rolls_back` | 0.025 | 0.034 | 0.023 | 0.025 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::1_maintenance::rejected_values_overflow_and_writer_settings_preserve_state` | 0.012 | 0.016 | 0.018 | 0.016 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::1_maintenance::single_table_filters_boolean_precedence_integer_limits_and_rollback` | 0.027 | 0.028 | 0.023 | 0.027 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::1_maintenance::single_table_integer_boundaries_and_public_view_are_enforced` | 0.016 | 0.015 | 0.018 | 0.016 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::2_vtab::catalog_identity_survives_vacuum_and_rename_rejects_modified_hooks` | 0.048 | 0.030 | 0.021 | 0.030 | 1 | floor, median x3 = 0.09 |
| `sqlite-ivm::2_vtab::ddl_rename_preserves_state_without_writes_and_drop_preserves_sources` | 0.048 | 0.028 | 0.028 | 0.028 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::2_vtab::ddl_rename_rollback_savepoints_and_failure_restore_usable_names` | 0.065 | 0.041 | 0.040 | 0.041 | 1 | floor, median x3 = 0.12 |
| `sqlite-ivm::2_vtab::defensive_shadow_protection_allows_source_dml_and_native_lifecycle` | 0.040 | 0.017 | 0.023 | 0.023 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::2_vtab::indexed_cursors_preserve_output_order_affinity_and_simultaneous_reads` | 0.029 | 0.019 | 0.023 | 0.023 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::3_relational::comma_joins_take_equality_keys_from_where` | 0.022 | 0.013 | 0.013 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::3_relational::deterministic_relational_mutations_and_type_contract` | 0.251 | 0.239 | 0.214 | 0.239 | 1 | floor, median x3 = 0.72 |
| `sqlite-ivm::3_relational::global_empty_aggregates_and_source_ddl` | 0.047 | 0.036 | 0.041 | 0.041 | 1 | floor, median x3 = 0.12 |
| `sqlite-ivm::3_relational::ignored_and_replaced_source_updates_preserve_arrangements` | 0.017 | 0.013 | 0.010 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::3_relational::narrow_source_column_order_changes_are_transactional` | 0.017 | 0.020 | 0.024 | 0.020 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::3_relational::null_recursive_keys_weighted_overflow_and_aggregate_exists` | 0.019 | 0.019 | 0.019 | 0.019 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::3_relational::nullable_text_composite_groups_outer_join_and_rollback` | 0.024 | 0.017 | 0.027 | 0.024 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::3_relational::recursive_shapes_bind_to_one_fixpoint_node_or_name_their_rejection` | 0.010 | 0.011 | 0.013 | 0.011 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::3_relational::relational_group_locality_and_ddl_preserve_btrees` | 0.015 | 0.016 | 0.016 | 0.016 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::3_relational::shared_circuit_states` | 0.219 | 0.248 | 0.236 | 0.236 | 1 | floor, median x3 = 0.71 |
| `sqlite-ivm::3_relational::unsupported_clause_combinations_fail_before_install` | 0.012 | 0.011 | 0.010 | 0.011 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::4_features::blobs_and_adjacent_floats_survive_trigger_transport` | 0.058 | 0.037 | 0.028 | 0.037 | 1 | floor, median x3 = 0.11 |
| `sqlite-ivm::4_features::collations_control_group_distinct_join_and_outer_predicates` | 0.084 | 0.059 | 0.085 | 0.084 | 1 | floor, median x3 = 0.25 |
| `sqlite-ivm::4_features::comma_join_arrangements_hold_side_row_counts_never_the_product` | 0.038 | 0.021 | 0.022 | 0.022 | 1 | floor, median x3 = 0.07 |
| `sqlite-ivm::4_features::comma_join_star_predicate_matches_plain_query` | 0.091 | 0.077 | 0.112 | 0.091 | 1 | floor, median x3 = 0.27 |
| `sqlite-ivm::4_features::feature_compositions_against_sqlite` | 1.320 | 1.343 | 1.326 | 1.326 | 4.0 | median x3 = 4.0, rounded to 4.0 |
| `sqlite-ivm::4_features::materialized_output_affinity_matches_ordinary_view_consumers` | 0.025 | 0.013 | 0.010 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::4_features::parenthesized_join_scopes_and_nonrecursive_union_cte` | 0.033 | 0.027 | 0.022 | 0.027 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::4_features::recursion_statement_count_is_linear_in_new_closure_rows` | 8.938 | 8.914 | 8.928 | 8.928 | 26.8 | median x3 = 26.8, rounded to 26.8 |
| `sqlite-ivm::4_features::recursive_delete_preserves_alternate_null_support_and_collated_roots` | 0.018 | 0.019 | 0.020 | 0.019 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::4_features::row1_binary_closure_with_cycles` | 0.036 | 0.030 | 0.036 | 0.036 | 1 | floor, median x3 = 0.11 |
| `sqlite-ivm::4_features::row2_parity_over_roots` | 0.040 | 0.029 | 0.033 | 0.033 | 1 | floor, median x3 = 0.10 |
| `sqlite-ivm::4_features::row3_filtered_step_with_two_joins_and_distinct` | 0.070 | 0.064 | 0.068 | 0.068 | 1 | floor, median x3 = 0.20 |
| `sqlite-ivm::4_features::row4_min_distance_aggregate_after_bounded_recursion` | 0.049 | 0.040 | 0.044 | 0.044 | 1 | floor, median x3 = 0.13 |
| `sqlite-ivm::4_features::row5_antijoin_and_exists_after_recursion` | 0.075 | 0.091 | 0.074 | 0.075 | 1 | floor, median x3 = 0.22 |
| `sqlite-ivm::4_features::row6_sequential_fixpoints_and_two_step_rules` | 0.092 | 0.082 | 0.076 | 0.082 | 1 | floor, median x3 = 0.25 |
| `sqlite-ivm::4_features::rows7_and_8_named_rejections_leave_no_state` | 0.014 | 0.008 | 0.012 | 0.012 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::5_transactions::cascades_generated_values_and_user_trigger_writes_compose` | 0.063 | 0.027 | 0.030 | 0.030 | 1 | floor, median x3 = 0.09 |
| `sqlite-ivm::5_transactions::deterministic_registered_scalars_and_rejected_volatile_functions` | 0.033 | 0.016 | 0.011 | 0.016 | 1 | floor, median x3 = 0.05 |
| `sqlite-ivm::5_transactions::recursive_member_tables_follow_savepoints_and_rollback` | 0.037 | 0.021 | 0.018 | 0.021 | 1 | floor, median x3 = 0.06 |
| `sqlite-ivm::5_transactions::wal_snapshots_writer_contention_and_failed_maintenance_are_atomic` | 0.047 | 0.035 | 0.037 | 0.037 | 1 | floor, median x3 = 0.11 |
| `sqlite-ivm::6_extension_load::extension_loads_through_its_entry_point` | 0.010 | 0.010 | 0.009 | 0.010 | 1 | floor, median x3 = 0.03 |
| `sqlite-ivm::6_extension_load::loaded_extension_maintains_a_view_end_to_end` | 0.008 | 0.010 | 0.006 | 0.008 | 1 | floor, median x3 = 0.02 |
| `sqlite-ivm::6_extension_load::loaded_extension_registers_the_create_function` | 0.008 | 0.008 | 0.007 | 0.008 | 1 | floor, median x3 = 0.02 |
| `sqlite-ivm::7_key_agreement::identity_round_trips_the_corpus` | 0.038 | 0.015 | 0.013 | 0.015 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::7_key_agreement::json_subtype_group_key_agrees_between_bulk_and_incremental_paths` | 0.035 | 0.013 | 0.008 | 0.013 | 1 | floor, median x3 = 0.04 |
| `sqlite-ivm::7_key_agreement::rust_key_after_expression_agrees_with_sql_key_sql` | 0.045 | 0.028 | 0.020 | 0.028 | 1 | floor, median x3 = 0.08 |
| `sqlite-ivm::8_group_limit::group_limit_matches_plain_sql_across_multiplicity_limit_and_offset` | 1.558 | 1.523 | 1.526 | 1.526 | 4.6 | median x3 = 4.6, rounded to 4.6 |
| `sqlite-ivm::8_group_limit::window_with_limit_reads_every_copy` | 0.118 | 0.103 | 0.102 | 0.103 | 1 | floor, median x3 = 0.31 |
| `sqlite-ivm::9_fixpoint_retraction::fixpoint_retraction_emits_the_stored_representative` | 0.056 | 0.029 | 0.031 | 0.031 | 1 | floor, median x3 = 0.09 |

Level 1 cap: 26.8s on the slowest leg leaves no budget under the 10-second law
that also clears a quiet-machine measurement, because that leg already runs
8.9s. The law is answered at the gate instead: the ceiling is 120s, and the
8.9s wall of `recursion_statement_count_is_linear_in_new_closure_rows` is
itself a named slow leg in R5.

The two legs this lane adds are measured the same way, in the same record run:
`10_growth::drain_statement_costs_match_the_pinned_counts` 0.048s and
`10_growth::phase_and_cost_growth_classes_hold` 0.084s, medians of three. Both
take the 1s floor, so neither needs an override.

### Tolerance, measured

Level 3 fires when a leg is above both `recorded x 1.5` and
`recorded + 0.150s`. The two thresholds come from the spread above:

| quantity | observed over three base runs |
|---|---|
| largest absolute spread | 0.047s (`13_statements_per_drain`, 0.083s to 0.130s) |
| largest relative spread | 250 percent (`11_intern::the_identity_splits...`, 0.010s to 0.034s) |
| slowest leg spread | 0.27 percent (`4_features::recursion_statement_count...`) |

A ratio alone flaps: the 0.010s leg moved 250 percent. An allowance alone is
blind: a small leg doubling stays inside 0.150s. Requiring both means the
0.010s leg fails only above 0.160s (16x recorded) and the 8.9s leg fails above
13.39s (1.5x recorded). The allowance is 3.2x the largest observed spread. The
incident's 1.7x sits at 15.2s on that leg, above the 13.39s limit, and R4 shows
that exact number firing.

### Level 3 split: pinned counts first, recorded wall second

The wall tolerance above is the residue, not the regression rail. The
regression rail is deterministic and it is already in the tree.

Deterministic legs, all in `tests/10_growth.rs`, all one run, all without a
tolerance band, because `4_counts.rs:78` and `5_sqlite.rs:90` say counts are
the same on every machine for the same input:

| leg | pins | catches |
|---|---|---|
| `drain_statement_costs_match_the_pinned_counts` | every statement the drain runs for eight views, as `vm_step/fullscan_step/events`, compared as a multiset per view against `tests/fixtures/2_statement_costs.json` (250 statements) | a statement that grows by any factor, including forty percent, and any statement that starts scanning |
| `phase_and_cost_growth_classes_hold` | the growth class of the `drain`, `node`, `fixpoint` and `round` spans between 8 and 128 source rows; the node span instance count of every kind at both sizes; the growth class of each view's `vm_step` | a phase that turns quadratic, and a node kind that starts making one span per row |

Measured classes at base: `drain`, `node`, `fixpoint` and `round` are Constant;
all eight views' `vm_step` is Linear. `refresh_statement_costs`, marked
`#[ignore]`, rewrites the fixture.

The timed legs stay for what counts cannot express: a wall moves with page
cache, I/O, lock waits and process start while every count holds still. Two
guards keep that residue from flapping. The record script discards a warmup
run, so a cold baseline cannot be committed. The gate builds every test binary
with `cargo nextest run --no-run` before the timed run, so a cold run cannot
reach the measurement either, and no test in the battery builds anything: the
one test that loads a native artifact skips when the artifact is absent, and
the scenarios that build it run after the check.

## R4: the rail seen to fail

A rail nobody has seen fail is not a rail. Three temporary probes, one per
level, each deleted before the rail landed. The probe file:

```
// Temporary probe for the timeout rail receipts (R4). Deleted before the rail
// lands; it exists only to be seen to fail at each of the three levels.
#[test]
fn rail_probe_within_budget() {
    std::thread::sleep(std::time::Duration::from_millis(900));
}
```

### Level 1, a per-test budget

Probe: two tests, one sleeping 300ms, one sleeping 300s. Budget 1s from the
default profile. The 300s sleep dies at 1.003s. Command and output, through the
gate:

```
$ bash scripts/9_verify.sh
 TERMINATING [>  1.000s] (─────) sqlite-ivm::zz_rail_probe rail_probe_over_budget
     TIMEOUT [   1.003s] (67/70) sqlite-ivm::zz_rail_probe rail_probe_over_budget
  stdout ───

    running 1 test

    (test timed out)

        PASS [   1.296s] (68/70) sqlite-ivm::4_features feature_compositions_against_sqlite
        PASS [   1.508s] (69/70) sqlite-ivm::8_group_limit group_limit_matches_plain_sql_across_multiplicity_limit_and_offset
        PASS [   8.789s] (70/70) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
────────────
     Summary [   8.912s] 70 tests run: 69 passed, 1 timed out, 0 skipped
     TIMEOUT [   1.003s] (67/70) sqlite-ivm::zz_rail_probe rail_probe_over_budget
error: test run failed
GATE_EXIT=100
```

### Level 2, the whole-battery ceiling

The real battery with the ceiling lowered to 2s, to see the ceiling kill a run
that the per-test budgets allow. Gate command:

```
$ TIMEOUT_RAIL_CEILING=2 bash scripts/9_verify.sh
  Cancelling due to signal: 1 test still running
     SIGTERM [   1.718s] (68/68) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
  stdout ───

    running 1 test

    (test aborted with signal 15: SIGTERM)

────────────
     Summary [   1.813s] 68 tests run: 67 passed, 1 failed, 0 skipped
     SIGTERM [   1.718s] (68/68) sqlite-ivm::4_features recursion_statement_count_is_linear_in_new_closure_rows
error: test run failed
gate: battery exceeded the 2s ceiling
GATE_EXIT=124
```

### Level 3, the recorded regression

Probe recorded at 0.309s, then slowed to 0.900s, still under its 1s budget.
The gate names the test, the observed wall, the recorded number, and both
thresholds:

```
$ bash scripts/9_verify.sh
timeout rail: regression sqlite-ivm::zz_rail_probe::rail_probe_within_budget 0.911s recorded 0.309s (over 0.464s ratio and 0.459s absolute)
timeout rail: 1 leg(s) above the recorded wall. Re-run scripts/timeout-rail-record.sh on a quiet machine only after the regression is understood.
GATE_EXIT=1
```

### Level 3 at the incident's own size

The same comparator against the real slow leg, with the recorded number divided
by 1.7 to stand in for a 1.7x regression. The leg's real wall is 8.852s:

```
timeout rail: regression sqlite-ivm::4_features::recursion_statement_count_is_linear_in_new_closure_rows 8.852s recorded 5.164s (over 7.746s ratio and 5.314s absolute)
timeout rail: 1 leg(s) above the recorded wall. Re-run scripts/timeout-rail-record.sh on a quiet machine only after the regression is understood.
exit 1
```

### The deterministic legs, seen to fail

Three perturbations, each reverted before the commit. The first is the incident
at forty percent, on one statement of one view:

```
$ python3 -c 'raise the map_view pin from 470/14/1 to 658/14/1'
$ cargo nextest run -E 'test(=drain_statement_costs_match_the_pinned_counts)'
    statement costs moved:
    map_view: 12 statements measured, 12 pinned
      measured only: ["470/14/1 x1"]
      pinned only:   ["658/14/1 x1"]
      heaviest measured: 470/14/1 WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce(max(__m),0) FROM temp.__ivm_out_1_2)) INSERT INTO main."map_view_state"(__key,c0) SELECT ... | 396/0/8 INSERT INTO temp.__ivm_out_2_0 VALUES(?1,?2,?3) | 110/7/1 ...
EXIT=100
```

The failure names the view, both multisets, and the statement with its SQL, so
the regressed statement is the first line of the output.

```
$ # expected class for the drain span changed from Constant to Linear
$ cargo nextest run -E 'test(=phase_and_cost_growth_classes_hold)'
assertion `left == right` failed: span drain grew Constant from 8 to 8 entries across a 16x input
CLASS_EXIT=100

$ # pinned node span instance count for kind map changed from 20 to 19
$ cargo nextest run -E 'test(=phase_and_cost_growth_classes_hold)'
assertion `left == right` failed: node span instances of kind map at 8 rows
  left: 20
 right: 19
INSTANCE_EXIT=100
```

## R5: the battery green, and what the rail costs

Three full gate runs with the rail on, `SQLITE3` pointed at a `sqlite3` built
with extension loading (see the note below), all six Bash scenarios included,
70 tests after the two deterministic legs:

```
$ SQLITE3=/opt/homebrew/opt/sqlite/bin/sqlite3 bash scripts/9_verify.sh
timeout rail: 70 tests and the battery within tolerance for Darwin arm64
     Summary [   8.918s] 70 tests run: 70 passed, 1 skipped      (run 1, real 9.58s)
     Summary [   8.900s] 70 tests run: 70 passed, 1 skipped      (run 2, real 9.57s)
     Summary [   8.877s] 70 tests run: 70 passed, 1 skipped      (run 3, real 9.47s)
exit 0 three times; the six scenarios end in PASS
```

The one skipped test is the ignored fixture refresh, which the gate never runs.
The two new legs cost 0.036s and 0.093s inside the battery, both under the 1s
floor, so neither needs an override.

The gate wall is the battery plus 0.4s to 0.6s of scenarios. Battery wall
without the rail, the old `cargo test --locked` invocation, three runs:
12.73s, 12.75s, 12.81s. Battery wall with the rail, nextest: 8.88s to 8.92s
above. The rail is about 3.8s cheaper than the line it replaces, because
nextest runs test binaries in parallel. The level 3 comparator costs 0.02s:
`python3 scripts/timeout-rail.py check` reads one JUnit file. The rail adds no
measurable wall, and the deterministic legs add 0.13s of it.

Environment note. Run without `SQLITE3`, the six Bash scenarios fail on this
machine: `/usr/bin/sqlite3` is Apple's 3.43.2 and refuses `load_extension`
(`Parse error near line 3: no such function: load_extension`). That failure is
older than this lane and unchanged by it, and `scripts/[1-6]_*.sh` are not
touched here. CI sets `SQLITE3` to Homebrew's sqlite3 on macOS and installs the
distro sqlite3 on Linux, so the scenarios run there.

## R6: the recorded files and their refresh

Two checked-in artifacts carry level 3, one deterministic and one timed.

`tests/fixtures/2_statement_costs.json` is the deterministic one: 250
statements, 4.4 KB, one `vm_step/fullscan_step/events` entry per distinct
statement per view, sorted. Refresh procedure, one paragraph: run
`cargo nextest run --run-ignored ignored-only -E 'test(=refresh_statement_costs)'`.
That test rebuilds the fixture from one drain of eight views over eight source
rows and writes it back. Commit the file only after the change to a statement's
work is understood, because a moved pin is the rail working, not the rail
failing. The numbers are SQLite's own opcode counts, so they do not drift with
the machine; they move only when a statement's work changes or when the pinned
`rusqlite` build changes.

`scripts/timeout-rail.tsv` is the timed one: one row per platform per test plus
one `BATTERY` row, all medians of three runs. The row in this commit is
`Darwin arm64`, 70 tests, battery 8.826s. The comparator uses the row for the
platform in `uname -sm` and prints a notice, leaving levels 1 and 2 in force,
when the platform has no row yet.

Refresh procedure for the timed file, one paragraph: on a quiet machine at the
sha whose walls are to be recorded, run `bash scripts/timeout-rail-record.sh`.
It runs the battery four times through `cargo nextest run` with the rail's own
config, discards run 1 as a warmup, copies the three JUnit reports that follow,
and writes the per-test median and the battery median for `uname -sm` into
`scripts/timeout-rail.tsv`. Commit the file. The discard exists because a cold
target dir or a cold page cache swings the first run far above the rest, and a
baseline taken from it would fire on the next warm run.

## R7: the diff

```
$ git diff --stat origin/main...HEAD
 .github/workflows/sqlite-ivm.yml      |  26 +-
 plans/costs/timeout-rail.md           | 658 ++++++++++++++++++++++++++++++++++
 scripts/9_verify.sh                   |  55 ++-
 scripts/nextest.toml                  |  37 ++
 scripts/timeout-rail-record.sh        |  32 ++
 scripts/timeout-rail.py               | 115 ++++++
 scripts/timeout-rail.tsv              |  73 ++++
 tests/10_growth.rs                    | 252 ++++++++++++-
 tests/fixtures/2_statement_costs.json | 272 ++++++++++++++
 9 files changed, 1514 insertions(+), 6 deletions(-)
```

Every path is owned: `.github/workflows/**`, `scripts/` (the repo has no
`justfile`, so `scripts/` is the gate), `tests/**` for the deterministic legs,
and the receipts page. Nothing under `src/`, `bench/`, or `archive/`.
`Cargo.toml` is untouched: the rail needs no harness dependency, only the
`cargo-nextest` binary that CI installs with `taiki-e/install-action@v2`, and
`hafley-observe`, which is already a dependency.

`tests/10_growth.rs` gains two tests, one ignored refresh test, a view list, a
pinned instance map and a helper. No existing test's assertions change; the
only edit to existing lines is the import. The fixture is generated, and R6
gives the command that regenerates it.

Two changes inside the workflow need naming. First, the verify job gains
`timeout-minutes: 30`, the outer bound over the gate's own 120s ceiling.
Second, the trigger. The workflow fired only on a change to
`.github/workflows/sqlite-ivm.yml`, on both `push` and `pull_request`. A clock
in a workflow that never runs on a code change catches nothing, so the paths
now name the code the gate covers: `src/**`, `tests/**`, `scripts/**`,
`bench/**`, `Cargo.toml`, `Cargo.lock`, and the workflow file itself. That
trigger is the one piece of this lane the parent may want to decide again.

