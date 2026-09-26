-- title: Party stats
-- question: For each trainer, how many Pokémon, what total level, and what lowest and highest level?
-- name: 1001 Red
-- name: 1002 Blue
-- name: 1003 Misty
-- name: 2010 Caterpie
-- name: 2016 Pidgey
-- name: 2025 Pikachu
-- name: 2120 Staryu
-- name: 2133 Eevee
-- node: 0 every Pokémon, its trainer and its level
-- node: 1 per trainer: Pokémon count, total level, lowest level, highest level
-- node: 2 how many Pokémon all trainers carry together
CREATE TABLE party_slot(pokemon INTEGER PRIMARY KEY, trainer INTEGER NOT NULL, level INTEGER NOT NULL);
CREATE VIEW party_stats AS
SELECT trainer, count(*) AS pokemon, sum(level) AS total_level, min(level) AS lowest_level, max(level) AS highest_level
FROM party_slot GROUP BY trainer;
CREATE VIEW pokemon_count AS SELECT count(*) AS pokemon FROM party_slot HAVING count(*) > 0;
-- step: Red has Pikachu at level 5 and Pidgey at level 7; Blue has Eevee at level 11
BEGIN;
INSERT INTO party_slot VALUES (2025,1001,5),(2016,1001,7),(2133,1002,11);
COMMIT;
-- step: Red catches Caterpie at level 3 and Blue trades Eevee to Red
BEGIN;
INSERT INTO party_slot VALUES (2010,1001,3);
UPDATE party_slot SET trainer=1001 WHERE pokemon=2133;
COMMIT;
-- step: A glitch sets Pidgey's level to -7
UPDATE party_slot SET level=-7 WHERE pokemon=2016;
-- step: Red releases Pikachu and Caterpie
BEGIN;
DELETE FROM party_slot WHERE pokemon IN (2025,2010);
COMMIT;
-- step: Red releases Pidgey and Eevee
DELETE FROM party_slot WHERE pokemon IN (2016,2133);
-- step: Misty catches Staryu, then turns the game off without saving
BEGIN;
INSERT INTO party_slot VALUES (2120,1003,9);
ROLLBACK;
