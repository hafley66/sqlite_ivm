-- title: Can Red Surf?
-- question: Which field moves can each trainer use outside battle?
-- name: 1001 Red
-- name: 1002 Blue
-- name: 1003 Misty
-- name: 1004 Brock
-- name: 1005 Erika
-- name: 2080 Slowbro
-- name: 2131 Lapras
-- name: 3019 Fly
-- name: 3057 Surf
-- name: 3070 Strength
-- name: 3148 Flash
-- node: 0 moves a trainer's ride pager lets them use
-- node: 1 which Pokémon each trainer carries
-- node: 2 which moves each Pokémon knows
-- node: 3 party members matched to the moves they know
-- node: 4 trainer and move, once per party member that knows it
-- node: 5 every reason a trainer can use a move
-- node: 6 can use: yes or no
CREATE TABLE party(trainer INTEGER NOT NULL, pokemon INTEGER NOT NULL, PRIMARY KEY(trainer, pokemon));
CREATE TABLE knows(pokemon INTEGER NOT NULL, move INTEGER NOT NULL, PRIMARY KEY(pokemon, move));
CREATE TABLE ride_pager(trainer INTEGER NOT NULL, move INTEGER NOT NULL, PRIMARY KEY(trainer, move));
CREATE VIEW can_use AS
SELECT trainer, move FROM ride_pager
UNION
SELECT p.trainer, k.move FROM party AS p JOIN knows AS k ON k.pokemon = p.pokemon;
-- step: Red carries Slowbro and Lapras, who both know Surf; Misty's ride pager calls up Flash
BEGIN;
INSERT INTO party VALUES (1001,2080),(1001,2131);
INSERT INTO knows VALUES (2080,3057),(2131,3057);
INSERT INTO ride_pager VALUES (1003,3148);
COMMIT;
-- step: Blue catches a Slowbro and Slowbro learns Strength
BEGIN;
INSERT INTO party VALUES (1002,2080);
INSERT INTO knows VALUES (2080,3070);
COMMIT;
-- step: Red's ride pager adds Strength
INSERT INTO ride_pager VALUES (1001,3070);
-- step: Red boxes Slowbro
DELETE FROM party WHERE trainer=1001 AND pokemon=2080;
-- step: Lapras forgets Surf
DELETE FROM knows WHERE pokemon=2131 AND move=3057;
-- step: Red's ride pager drops Strength
DELETE FROM ride_pager WHERE trainer=1001 AND move=3070;
-- step: Brock adds Fly to a ride pager, then reloads the save point
BEGIN;
SAVEPOINT discarded;
INSERT INTO ride_pager VALUES (1004,3019);
ROLLBACK TO discarded;
RELEASE discarded;
COMMIT;
-- step: Erika catches a Slowbro, then turns the game off without saving
BEGIN;
INSERT INTO party VALUES (1005,2080);
ROLLBACK;
-- step: Slowbro forgets Strength and learns Flash
UPDATE knows SET move=3148 WHERE pokemon=2080 AND move=3070;
