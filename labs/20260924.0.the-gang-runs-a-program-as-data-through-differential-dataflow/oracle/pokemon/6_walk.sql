-- title: Where can Red walk?
-- question: From each place, which places can a trainer reach on foot while Snorlax sleeps in the way?
-- name: 4011 Route 11
-- name: 4012 Route 12
-- name: 4013 Route 13
-- name: 4015 Route 15
-- name: 4101 Pallet Town
-- name: 4102 Viridian City
-- name: 4103 Pewter City
-- name: 4104 Cerulean City
-- name: 4105 Vermilion City
-- name: 4106 Lavender Town
-- name: 4108 Fuchsia City
-- name: 4109 Saffron City
-- name: 4201 Diglett's Cave
-- name: 4202 Safari Zone
-- node: 0 every one-way road from one place to the next
-- node: 1 places where Snorlax sleeps
-- node: 2 roads whose end is not blocked by Snorlax
-- node: 3 places reachable so far
-- node: 4 a reachable place joined to an open road leaving it
-- node: 5 start place and the new place that road reaches
-- node: 6 every place reachable from each start, by one road or more
CREATE TABLE road(from_town INTEGER NOT NULL, to_town INTEGER NOT NULL, PRIMARY KEY(from_town, to_town));
CREATE TABLE snorlax_blocks(town INTEGER PRIMARY KEY);
CREATE VIEW can_walk AS WITH RECURSIVE r(from_town, to_town) AS (
  SELECT road.from_town, road.to_town FROM road WHERE NOT EXISTS (SELECT 1 FROM snorlax_blocks AS s WHERE s.town = road.to_town)
  UNION
  SELECT r.from_town, road.to_town FROM r JOIN road ON road.from_town = r.to_town
  WHERE NOT EXISTS (SELECT 1 FROM snorlax_blocks AS s WHERE s.town = road.to_town)
) SELECT from_town, to_town FROM r;
-- step: Route 11 leads to Lavender Town through Vermilion and Saffron, Lavender Town to Route 12, Route 12 back to Route 11 and on to Route 13
+ road 4011 4106
+ road 4106 4012
+ road 4012 4011
+ road 4012 4013
-- step: Snorlax falls asleep on Route 12
+ snorlax_blocks 4012
-- step: Red wakes Snorlax on Route 12 with the Poké Flute
- snorlax_blocks 4012
-- step: The road from Lavender Town onto Route 12 closes
- road 4106 4012
-- step: Pewter City reaches Vermilion City two ways, through Cerulean City or through Diglett's Cave
+ road 4103 4104
+ road 4104 4105
+ road 4103 4201
+ road 4201 4105
-- step: The road from Cerulean City to Vermilion City closes
- road 4104 4105
-- step: Route 1 runs both ways between Pallet Town and Viridian City
+ road 4101 4102
+ road 4102 4101
-- step: A road from Saffron City back into Saffron City appears, which Kanto does not have
+ road 4109 4109
-- step: The Saffron City loop road is removed
- road 4109 4109
-- step: Route 1 from Pallet Town to Viridian City closes and reopens in one step
- road 4101 4102
+ road 4101 4102
-- step: The road from Lavender Town onto Route 12 reopens
+ road 4106 4012
-- step: Fuchsia City and the Safari Zone connect both ways, and Route 15 leads into Fuchsia City
+ road 4108 4202
+ road 4202 4108
+ road 4015 4108
-- step: The road from Route 15 into Fuchsia City closes
- road 4015 4108
-- step: Route 15 reopens into Fuchsia City while the Safari Zone entrance closes
+ road 4015 4108
- road 4108 4202
-- step: The Safari Zone entrance reopens while its exit closes and reopens in one step
+ road 4108 4202
- road 4202 4108
+ road 4202 4108
