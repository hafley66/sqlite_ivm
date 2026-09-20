-- The golden compass. One file: setup, one view that crosses every operator
-- kind, the writes that exercise every SQLite event, an oracle for each
-- check, and a verdict. Run:
--
--   sqlite3 -bail :memory: ".param set @extension <path>" ".read tests/golden/compass.sql"
--
-- or through tests/12_compass.rs, which substitutes the extension path and
-- asserts the PASS line. A failing check raises, so -bail stops at the first.
--
-- Rows are few on purpose. Every count below is readable by hand.

SELECT load_extension(@extension);
PRAGMA recursive_triggers = ON;
PRAGMA trusted_schema = ON;

-- ---------------------------------------------------------------- sources
-- The trap values, once each: integer 1 against real 1.0, two NULLs, a blob,
-- an exact repeat, a self-referencing edge table for the recursive CTE.
CREATE TABLE customers(customer_id INTEGER PRIMARY KEY, region TEXT, tier INTEGER);
CREATE TABLE orders(order_id INTEGER PRIMARY KEY, customer_id INTEGER, amount REAL, note);
CREATE TABLE referrals(referrer INTEGER, referred INTEGER);

INSERT INTO customers VALUES
  (1, 'west', 1),
  (2, 'west', 2),
  (3, NULL,   1),
  (4, 'east', 1);
INSERT INTO orders VALUES
  (10, 1, 1.0,  'a'),
  (11, 1, 1,    'a'),
  (12, 2, NULL, x'00'),
  (13, 3, 2.5,  NULL),
  (14, 4, 2.5,  NULL);
INSERT INTO referrals VALUES (1, 2), (2, 3), (4, 1);

-- ---------------------------------------------------------------- the view
-- Map, Join, Group with HAVING, window, DISTINCT, top-k, recursive CTE, and a
-- subquery in FROM. One view, so one transaction drives every node kind.
CREATE VIRTUAL TABLE compass USING sqlite_ivm('
  WITH RECURSIVE downstream(root, customer_id, depth) AS (
    SELECT referrer, referred, 1 FROM referrals
    UNION
    SELECT d.root, r.referred, d.depth + 1
      FROM downstream d JOIN referrals r ON r.referrer = d.customer_id
     WHERE d.depth < 8
  ),
  spend AS (
    SELECT c.customer_id, c.region, c.tier,
           count(*) AS order_count,
           sum(o.amount) AS revenue,
           count(*) FILTER (WHERE o.amount IS NULL) AS unpriced
      FROM orders o JOIN customers c USING (customer_id)
     GROUP BY c.customer_id, c.region, c.tier
    HAVING count(*) >= 1
  ),
  reach AS (
    SELECT root, count(*) AS reach FROM downstream GROUP BY root
  )
  SELECT DISTINCT
         s.region,
         s.customer_id,
         s.order_count,
         s.revenue,
         s.unpriced,
         rank() OVER (PARTITION BY s.region ORDER BY s.revenue DESC, s.customer_id) AS revenue_rank,
         coalesce(r.reach, 0) AS reach
    FROM spend s LEFT JOIN reach r ON r.root = s.customer_id
   WHERE s.tier <= 2
   ORDER BY s.region, s.customer_id
   LIMIT 10
');

-- ---------------------------------------------------------------- oracle
-- The same query as a plain view. Every check compares the two both ways.
CREATE TEMP VIEW compass_oracle AS
  WITH RECURSIVE downstream(root, customer_id, depth) AS (
    SELECT referrer, referred, 1 FROM referrals
    UNION
    SELECT d.root, r.referred, d.depth + 1
      FROM downstream d JOIN referrals r ON r.referrer = d.customer_id
     WHERE d.depth < 8
  ),
  spend AS (
    SELECT c.customer_id, c.region, c.tier,
           count(*) AS order_count,
           sum(o.amount) AS revenue,
           count(*) FILTER (WHERE o.amount IS NULL) AS unpriced
      FROM orders o JOIN customers c USING (customer_id)
     GROUP BY c.customer_id, c.region, c.tier
    HAVING count(*) >= 1
  ),
  reach AS (
    SELECT root, count(*) AS reach FROM downstream GROUP BY root
  )
  SELECT DISTINCT
         s.region,
         s.customer_id,
         s.order_count,
         s.revenue,
         s.unpriced,
         rank() OVER (PARTITION BY s.region ORDER BY s.revenue DESC, s.customer_id) AS revenue_rank,
         coalesce(r.reach, 0) AS reach
    FROM spend s LEFT JOIN reach r ON r.root = s.customer_id
   WHERE s.tier <= 2
   ORDER BY s.region, s.customer_id
   LIMIT 10;

-- ---------------------------------------------------------------- checks
-- One row per check. `ok` is 1 or 0. The verdict at the end raises on any 0.
CREATE TEMP TABLE checks(seq INTEGER PRIMARY KEY, name TEXT NOT NULL, ok INTEGER NOT NULL);

-- Bag equality in both directions plus equal cardinality, so a duplicate row
-- on one side cannot hide behind EXCEPT's set semantics.
CREATE TEMP VIEW compass_agrees AS
  SELECT (SELECT count(*) FROM (SELECT * FROM compass EXCEPT SELECT * FROM compass_oracle)) = 0
     AND (SELECT count(*) FROM (SELECT * FROM compass_oracle EXCEPT SELECT * FROM compass)) = 0
     AND (SELECT count(*) FROM compass) = (SELECT count(*) FROM compass_oracle) AS ok;

INSERT INTO checks(name, ok) SELECT 'after create', ok FROM compass_agrees;

-- Hand-readable expectation for the starting state, so a wrong oracle cannot
-- agree with a wrong view.
INSERT INTO checks(name, ok) SELECT 'starting rows by hand',
  (SELECT count(*) FROM compass) = 4
  AND (SELECT order_count FROM compass WHERE customer_id = 1) = 2
  AND (SELECT revenue FROM compass WHERE customer_id = 1) = 2.0
  AND (SELECT unpriced FROM compass WHERE customer_id = 2) = 1
  AND (SELECT revenue_rank FROM compass WHERE customer_id = 1) = 1
  AND (SELECT reach FROM compass WHERE customer_id = 4) = 3
  AND (SELECT region FROM compass WHERE customer_id = 3) IS NULL;

-- ---------------------------------------------------------------- events
-- One transaction that draws every callback: multi-row INSERT, DELETE, UPDATE,
-- a user SAVEPOINT rolled back, a RELEASE, then COMMIT.
BEGIN;
INSERT INTO orders VALUES (15, 2, 100, 'big'), (16, 4, 0.5, NULL);
SAVEPOINT half;
DELETE FROM orders WHERE order_id = 10;
UPDATE customers SET region = 'east' WHERE customer_id = 2;
INSERT INTO referrals VALUES (3, 4);
ROLLBACK TO half;
RELEASE half;
UPDATE orders SET amount = 3.0 WHERE order_id = 13;
DELETE FROM referrals WHERE referrer = 4;
COMMIT;

INSERT INTO checks(name, ok) SELECT 'after savepoint transaction', ok FROM compass_agrees;
INSERT INTO checks(name, ok) SELECT 'rolled back work is absent',
  (SELECT revenue FROM compass WHERE customer_id = 1) = 2.0
  AND (SELECT region FROM compass WHERE customer_id = 2) = 'west'
  AND (SELECT reach FROM compass WHERE customer_id = 4) = 0
  AND (SELECT revenue FROM compass WHERE customer_id = 3) = 3.0;

-- A whole-transaction rollback leaves nothing behind.
BEGIN;
INSERT INTO orders VALUES (17, 1, 9, NULL);
DELETE FROM customers WHERE customer_id = 4;
ROLLBACK;
INSERT INTO checks(name, ok) SELECT 'after rollback', ok FROM compass_agrees;

-- Autocommit statements, one row each, the common case.
INSERT INTO orders VALUES (18, 3, 1, 'x');
DELETE FROM orders WHERE order_id = 12;
UPDATE customers SET tier = 3 WHERE customer_id = 2;
INSERT INTO checks(name, ok) SELECT 'after autocommit statements', ok FROM compass_agrees;
INSERT INTO checks(name, ok) SELECT 'tier filter dropped customer 2',
  (SELECT count(*) FROM compass WHERE customer_id = 2) = 0;

-- Exact repeat of an existing row image: multiplicity, not a new arrangement row.
INSERT INTO orders VALUES (19, 3, 1, 'x');
INSERT INTO checks(name, ok) SELECT 'after exact repeat', ok FROM compass_agrees;

-- ---------------------------------------------------------------- storage laws
-- Deterministic counts over the arrangements. Table names come from the
-- catalog, so a renumbered plan does not silently skip the check.
CREATE TEMP VIEW join_inputs AS
  SELECT object_name FROM __ivm_objects
   WHERE view_name = 'compass' AND object_type = 'table'
     AND object_name LIKE 'compass_op%'
     AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name = '__r');

-- Every arrangement stores its tuple once: no column beyond __k, __r, __n and c<i>.
INSERT INTO checks(name, ok) SELECT 'no stored copy of the tuple',
  NOT EXISTS (
    SELECT 1 FROM join_inputs j, pragma_table_info(j.object_name) p
     WHERE p.name NOT IN ('__k', '__r', '__n') AND p.name NOT GLOB 'c[0-9]*');

-- The rebuilt-hash rail and the empty-delta rail need one statement per
-- arrangement width, so tests/12_compass.rs runs them over this same database.

-- ---------------------------------------------------------------- verdict
-- raise() only works inside a trigger, so the verdict is an insert into a
-- table whose trigger refuses any failure count above zero.
CREATE TEMP TABLE verdict(failures INTEGER NOT NULL);
CREATE TEMP TRIGGER verdict_refuses_failures BEFORE INSERT ON verdict
  WHEN NEW.failures > 0
BEGIN
  SELECT raise(FAIL, 'compass failed');
END;
.mode list
SELECT name || ': ' || CASE ok WHEN 1 THEN 'ok' ELSE 'FAIL' END FROM checks ORDER BY seq;
INSERT INTO verdict SELECT count(*) FROM checks WHERE ok = 0;
SELECT 'PASS ' || count(*) || ' checks' FROM checks;
