-- expect-error: Unsupported("Delay")
CREATE TABLE source(c0 INTEGER PRIMARY KEY);
CREATE VIEW previous AS SELECT c0 FROM source;
-- step: first
+ source 1
