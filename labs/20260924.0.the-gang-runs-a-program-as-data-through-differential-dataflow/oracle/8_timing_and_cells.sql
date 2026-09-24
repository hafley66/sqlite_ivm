-- program: 0_access
-- S-T and S-I: one large frontier, adjacent frontiers stay separate, rows equal across relations, i64 extremes.
CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
CREATE VIEW access AS
SELECT person, resource FROM direct_grant
UNION
SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team;
-- step: base
+ permission 10 100
-- step: d11_two_thousand_rows_one_frontier
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
INSERT INTO membership SELECT i, 10 FROM n;
-- step: d10_insert_then
+ direct_grant 5 500
-- step: d10_delete_next_frontier
- direct_grant 5 500
-- step: d11_two_thousand_rows_retracted
DELETE FROM membership WHERE team = 10;
-- step: d14_equal_rows_in_three_relations
+ membership 1 10
+ permission 1 10
+ direct_grant 1 10
-- step: d15_cells_not_concatenated
+ direct_grant 1 23
+ direct_grant 12 3
-- step: d16_i64_extremes
+ direct_grant -1 0
+ direct_grant 9223372036854775807 -9223372036854775808
-- step: d16_extremes_joined
+ membership -9223372036854775808 9223372036854775807
+ permission 9223372036854775807 -1
