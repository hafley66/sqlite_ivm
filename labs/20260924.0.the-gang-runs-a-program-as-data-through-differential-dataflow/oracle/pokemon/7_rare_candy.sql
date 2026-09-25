-- title: Rare Candy to level 64
-- question: Feeding one Rare Candy at a time, which levels does a Pokémon pass from its start level up to the level 64 cap, and does any reach the cap?
-- node: 0 start levels of the Pokémon being fed
-- node: 1 levels passed so far
-- node: 2 one Rare Candy more: the next level, only below the cap of 64
-- node: 3 start levels plus every level one Rare Candy past a level already passed
-- node: 4 levels passed that sit exactly at the cap of 64
CREATE TABLE start_level(level INTEGER PRIMARY KEY);
CREATE VIEW levels_passed AS WITH RECURSIVE r(level) AS (
  SELECT level FROM start_level UNION SELECT level + 1 FROM r WHERE level < 64
) SELECT level FROM r;
CREATE VIEW at_cap AS WITH RECURSIVE r(level) AS (
  SELECT level FROM start_level UNION SELECT level + 1 FROM r WHERE level < 64
) SELECT level FROM r WHERE level = 64;
-- step: A glitch Pokémon at level 0 is fed Rare Candy up to the cap
+ start_level 0
-- step: A second Pokémon starts at level 60, inside the first one's climb
+ start_level 60
-- step: The level 0 Pokémon is released and the level 60 climb stays
- start_level 0
-- step: A level 70 Pokémon arrives, already past the cap
+ start_level 70
-- step: Both remaining Pokémon are released
- start_level 60
- start_level 70
