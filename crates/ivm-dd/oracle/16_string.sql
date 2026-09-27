-- SQL recompute oracle for both str.cons modes and str.nil.
CREATE TABLE name(c0 TEXT PRIMARY KEY);
CREATE TABLE word(c0 TEXT PRIMARY KEY);
CREATE VIEW greeting AS SELECT 'hi ' || c0 FROM name;
CREATE VIEW split AS SELECT substr(c0,1,1), substr(c0,2) FROM word WHERE c0 <> '';
CREATE VIEW empty AS SELECT '' FROM word;
CREATE VIEW ordering AS SELECT name.c0, word.c0 FROM name CROSS JOIN word WHERE name.c0 < word.c0 COLLATE BINARY;
CREATE VIEW roundtrip AS SELECT c0 FROM word WHERE c0 <> '';
