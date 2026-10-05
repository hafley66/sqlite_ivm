-- Waves of `g` (from `edge`, minus out-edges of reached `cut` nodes), `reach` nested per wave.
-- One rewrite reaches the fixed point, so the view is the closed form over `edge`.
CREATE TABLE edge(a INTEGER, b INTEGER, PRIMARY KEY (a, b));
CREATE TABLE root(n INTEGER PRIMARY KEY);
CREATE TABLE cut(n INTEGER PRIMARY KEY);
CREATE VIEW g AS
  WITH RECURSIVE reach(n) AS (SELECT n FROM root UNION SELECT e.b FROM reach r JOIN edge e ON e.a = r.n)
  SELECT a, b FROM edge WHERE NOT (a IN (SELECT n FROM cut) AND a IN (SELECT n FROM reach));
-- step: f0_chain_cut_at_3
+ edge 1 2
+ edge 2 3
+ edge 3 4
+ edge 4 5
+ root 1
+ cut 3
-- step: f1_cut_moves_to_2
- cut 3
+ cut 2
-- step: f2_second_cut_reached
+ cut 4
-- step: f3_root_reaches_4
+ root 4
-- step: f4_chain_broken
- edge 1 2
-- step: f5_cycle
+ edge 5 1
+ edge 1 2
-- step: f6_all_roots_removed
- root 1
- root 4
