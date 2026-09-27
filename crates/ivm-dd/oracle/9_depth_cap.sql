-- S-N5: the Sprefa depth cap is a data filter inside the recursion; IntAdd in the step; a 64-deep chain.
CREATE TABLE seed(x INTEGER PRIMARY KEY);
CREATE VIEW n AS WITH RECURSIVE r(x) AS (
  SELECT x FROM seed UNION SELECT x + 1 FROM r WHERE x < 64
) SELECT x FROM r;
CREATE VIEW exceeded AS WITH RECURSIVE r(x) AS (
  SELECT x FROM seed UNION SELECT x + 1 FROM r WHERE x < 64
) SELECT x FROM r WHERE x = 64;
-- step: f0_chain_of_65
+ seed 0
-- step: f1_second_seed_inside_chain
+ seed 60
-- step: f2_first_seed_gone_tail_stays
- seed 0
-- step: f3_seed_past_cap
+ seed 70
-- step: f4_all_gone
- seed 60
- seed 70
