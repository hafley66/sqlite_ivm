-- title: Party size
-- question: For each trainer, how many Pokémon and what total level?
-- name: 1001 Red
-- name: 1002 Blue
-- name: 2000 MissingNo.
-- name: 2025 Pikachu
-- name: 2133 Eevee
-- node: 0 every Pokémon, its trainer and its level
-- node: 1 per trainer: Pokémon count and total level
CREATE TABLE party_slot(pokemon INTEGER PRIMARY KEY, trainer INTEGER NOT NULL, level INTEGER NOT NULL);
CREATE VIEW party_size AS SELECT trainer, count(*) AS pokemon, sum(level) AS total_level FROM party_slot GROUP BY trainer;
-- step: Red has Pikachu at level 5 and a glitched MissingNo. at level -5; Blue has Eevee at level 4
+ party_slot 2025 1001 5
+ party_slot 2000 1001 -5
+ party_slot 2133 1002 4
-- step: Blue trades Eevee to Red
- party_slot 2133 1002 4
+ party_slot 2133 1001 4
-- step: Red releases Pikachu, MissingNo. and Eevee
- party_slot 2025 1001 5
- party_slot 2000 1001 -5
- party_slot 2133 1001 4
