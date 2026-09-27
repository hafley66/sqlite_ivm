-- title: Who is left to battle?
-- question: Which trainers stand on a route past Route 10 and have no win recorded against them?
-- name: 1001 Hiker
-- name: 1002 Lass
-- name: 1003 Bug Catcher
-- name: 4005 Route 5
-- name: 4010 Route 10
-- name: 4020 Route 20
-- name: 4022 Route 22
-- name: 4024 Route 24
-- node: 0 which trainer stands on which route
-- node: 1 trainers on a route numbered past Route 10
-- node: 2 every win recorded against a trainer, with its prize money
-- node: 3 trainers past Route 10 with no win recorded against them
CREATE TABLE trainer_on_route(trainer INTEGER NOT NULL, route INTEGER NOT NULL, PRIMARY KEY(trainer, route));
CREATE TABLE beaten(trainer INTEGER NOT NULL, prize INTEGER NOT NULL, PRIMARY KEY(trainer, prize));
CREATE VIEW rematch AS
SELECT trainer, route FROM trainer_on_route AS t
WHERE route > 4010 AND NOT EXISTS (SELECT 1 FROM beaten AS b WHERE b.trainer = t.trainer);
-- step: Hiker waits on Route 10, Lass on Route 20, Bug Catcher on Route 24; Red beats Lass for 100
+ trainer_on_route 1001 4010
+ trainer_on_route 1002 4020
+ trainer_on_route 1003 4024
+ beaten 1002 100
-- step: The record of Red's win over Lass is erased
- beaten 1002 100
-- step: Red beats Bug Catcher for 300
+ beaten 1003 300
-- step: Lass walks from Route 20 to Route 5
- trainer_on_route 1002 4020
+ trainer_on_route 1002 4005
-- step: Lass also stands on Route 22
+ trainer_on_route 1002 4022
-- step: Red beats Lass twice more, for 200 and for 201
+ beaten 1002 200
+ beaten 1002 201
-- step: The win over Lass for 200 is erased
- beaten 1002 200
-- step: The win over Lass for 201 is erased
- beaten 1002 201
