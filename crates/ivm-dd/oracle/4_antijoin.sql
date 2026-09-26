-- S-N1: antijoin both directions, filter crossing, right side with multiplicity 2 on one key.
CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
CREATE VIEW lonely AS
SELECT person, team FROM membership AS m
WHERE team > 10 AND NOT EXISTS (SELECT 1 FROM direct_grant AS g WHERE g.person = m.person);
-- step: f0_base
+ membership 1 10
+ membership 2 20
+ membership 3 30
+ direct_grant 2 100
-- step: f1_right_retract_reveals_left
- direct_grant 2 100
-- step: f2_right_insert_hides_left
+ direct_grant 3 300
-- step: f3_update_crosses_filter_out
- membership 2 20
+ membership 2 5
-- step: f4_insert_passes_filter
+ membership 2 25
-- step: d27_right_multiplicity_two
+ direct_grant 2 200
+ direct_grant 2 201
-- step: d28_right_two_to_one
- direct_grant 2 200
-- step: d28_right_one_to_zero
- direct_grant 2 201
