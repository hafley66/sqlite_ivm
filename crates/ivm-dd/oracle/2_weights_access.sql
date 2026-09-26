-- program: 0_access
-- S-W on the access program; raw steps send exactly the listed frontier.
CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
CREATE VIEW access AS
SELECT person, resource FROM direct_grant
UNION
SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team;
-- step: d13_empty_sources_empty_frontier
-- step: d13_first_rows_one_side_only
+ permission 10 100
-- step: d9_key_support_on_both_sides
+ membership 1 10
+ membership 2 10
+ permission 10 101
-- step: d9_retract_one_partner_of_shared_key
- permission 10 100
-- step: d1_cancel_new_row_in_batch
+ membership 6 10
- membership 6 10
-- step: d2_delete_reinsert_live_row_in_batch
- membership 2 10
+ membership 2 10
-- step: d4_delete_absent_row
- membership 9 10
-- step: d5_insert_after_absent_delete
+ membership 9 10
-- step: d6_insert_present_row_rejected
+ direct_grant 7 700
+ membership 1 10
-- expect-error: PresentInsert
-- step: d8_setup
+ membership 8 80
+ permission 80 800
-- step: d8_both_join_sides_retracted_in_batch
- membership 8 80
- permission 80 800
-- step: c3_empty_frontier_after_data
