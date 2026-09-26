-- title: Who leads?
-- question: Which Pokémon leads each party, what is the party's highest level, and which two Pokémon open a Double Battle?
-- name: 1001 Red
-- name: 1002 Blue
-- name: 2001 Bulbasaur
-- name: 2004 Charmander
-- name: 2007 Squirtle
-- name: 2016 Pidgey
-- name: 2019 Rattata
-- node: 0 every Pokémon, its trainer and its level
-- node: 1 lead: highest level per trainer, ties go to the lower Pokédex number
-- node: 2 one highest-level Pokémon per trainer, any of the tied ones
-- node: 3 trainer and that highest level
-- node: 4 highest level per trainer, once
-- node: 5 Double Battle pair: the two highest levels per trainer, ties go to the lower Pokédex number
CREATE TABLE party_slot(pokemon INTEGER PRIMARY KEY, trainer INTEGER NOT NULL, level INTEGER NOT NULL);
CREATE VIEW lead AS SELECT pokemon, trainer, level FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY trainer ORDER BY level DESC, pokemon ASC) AS rn FROM party_slot) WHERE rn = 1;
CREATE VIEW top_level AS SELECT DISTINCT trainer, level FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY trainer ORDER BY level DESC) AS rn FROM party_slot) WHERE rn = 1;
CREATE VIEW double_battle_pair AS SELECT pokemon, trainer, level FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY trainer ORDER BY level DESC, pokemon ASC) AS rn FROM party_slot) WHERE rn <= 2;
-- step: Red has Bulbasaur and Charmander at level 9 and Squirtle at level 4; Blue has Pidgey at level 7
+ party_slot 2001 1001 9
+ party_slot 2004 1001 9
+ party_slot 2007 1001 4
+ party_slot 2016 1002 7
-- step: Red boxes Bulbasaur
- party_slot 2001 1001 9
-- step: Red boxes Charmander
- party_slot 2004 1001 9
-- step: Blue trades Pidgey to Red
- party_slot 2016 1002 7
+ party_slot 2016 1001 7
-- step: Red catches Rattata at level 7, tying Pidgey
+ party_slot 2019 1001 7
