-- Non-monotone LetRec: each start walks up by one per round until it stands on a stop. The body
-- retracts: `start` enters only while `cur` is empty (antijoin on the recursive right input), and a
-- walker's old position leaves `cur` as it steps (negate + threshold). `size` counts `cur` (reduce
-- over a variable). The view is the fixed point in closed form: every start reaches the least stop
-- at or above it. Every step keeps a stop above each start, so the limit (8) is never reached.
CREATE TABLE start(n INTEGER PRIMARY KEY);
CREATE TABLE stop(n INTEGER PRIMARY KEY);
CREATE VIEW cur AS SELECT DISTINCT (SELECT min(t.n) FROM stop t WHERE t.n >= s.n) AS n FROM start s;
CREATE VIEW size AS SELECT count(*) AS c FROM cur HAVING count(*) > 0;
-- step: f0_one_walker
+ start 1
+ stop 4
-- step: f1_second_walker_merges
+ start 2
-- step: f2_nearer_stop
+ stop 3
-- step: f3_walker_on_a_stop
+ start 4
-- step: f4_stop_removed
- stop 3
-- step: f5_all_starts_removed
- start 1
- start 2
- start 4
-- step: f6_restart
+ start 5
+ stop 6
+ stop 5
-- step: f7_delete_readd_in_batch
- stop 5
+ stop 5
-- step: f8_stop_moves
- stop 5
+ stop 7
