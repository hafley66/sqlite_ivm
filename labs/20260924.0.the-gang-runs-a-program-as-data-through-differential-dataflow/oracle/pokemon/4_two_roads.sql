-- title: Two roads away
-- question: Which towns are exactly two roads apart, counting each way to get there?
-- name: 4104 Cerulean City
-- name: 4106 Lavender Town
-- name: 4109 Saffron City
-- node: 0 every one-way road from one town to the next
-- node: 1 a first road joined to a second road that starts where the first one ends
-- node: 2 start town and end town of the two-road trip
CREATE TABLE road(from_town INTEGER NOT NULL, to_town INTEGER NOT NULL, PRIMARY KEY(from_town, to_town));
CREATE VIEW two_roads AS SELECT a.from_town, b.to_town FROM road AS a JOIN road AS b ON a.to_town = b.from_town;
-- step: Kanto before any road is built
-- step: Route 5 from Cerulean to Saffron, Route 8 from Saffron to Lavender, and a Saffron-to-Saffron road Kanto does not have
+ road 4104 4109
+ road 4109 4106
+ road 4109 4109
-- step: The Saffron-to-Saffron road is removed
- road 4109 4109
-- step: Routes 9 and 10 lead from Lavender Town back to Cerulean City
+ road 4106 4104
-- step: Route 5 closes; Routes 9 and 10 now also lead from Cerulean City to Lavender Town
- road 4104 4109
+ road 4104 4106
