#![cfg(not(feature = "extension"))]

use rusqlite::{Connection, Result};
use sqlite_ivm::{extension::register, query::quote};

const JOIN: &str = "SELECT i.group_id AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i JOIN dimensions d ON i.join_key=d.join_key GROUP BY i.group_id";
const RIGHT_GROUP: &str = "SELECT d.bucket AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i INNER JOIN dimensions d ON d.join_key=i.join_key GROUP BY d.bucket";

fn database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
        CREATE TABLE items(id INTEGER PRIMARY KEY, join_key INTEGER NOT NULL, group_id INTEGER NOT NULL, amount INTEGER NOT NULL);
        CREATE TABLE dimensions(id INTEGER PRIMARY KEY, join_key INTEGER NOT NULL, factor INTEGER NOT NULL, bucket INTEGER NOT NULL);")?;
    register(&db)?;
    Ok(db)
}

fn install(db: &Connection, name: &str, sql: &str) -> Result<()> {
    let actual: String =
        db.query_row("SELECT sqlite_ivm_create(?1,?2)", [name, sql], |r| r.get(0))?;
    assert_eq!(actual, name);
    Ok(())
}

fn rows(db: &Connection, sql: &str) -> Result<Vec<(i64, i64, i64)>> {
    db.prepare(sql)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

fn verify(db: &Connection, name: &str, sql: &str) -> Result<()> {
    assert_eq!(
        rows(db, &format!("SELECT * FROM {} ORDER BY 1", quote(name)))?,
        rows(db, &format!("{sql} ORDER BY 1"))?,
        "{name}"
    );
    let stage = quote(&format!("{name}_delta"));
    assert_eq!(
        db.query_row(&format!("SELECT COUNT(*) FROM {stage}"), [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn managed_drop_preserves_sources_and_other_views_and_rolls_back() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "INSERT INTO items VALUES(1,10,4,6),(2,10,4,8);
        INSERT INTO dimensions VALUES(1,10,25,100)",
    )?;
    install(&db, "earnings", JOIN)?;
    let eligible = JOIN.replace(" GROUP BY", " WHERE i.amount>=7 GROUP BY");
    install(&db, "eligible_earnings", &eligible)?;
    assert_eq!(
        db.query_row(
            "SELECT query_sql FROM __ivm_views WHERE name='eligible_earnings'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        eligible
    );
    assert_eq!(db.prepare("SELECT source_ordinal,table_name FROM __ivm_sources WHERE view_name='earnings' ORDER BY source_ordinal")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>>>()?,
        vec![(0,"items".into()),(1,"dimensions".into())]);
    assert_eq!(db.prepare("SELECT source_ordinal,column_name FROM __ivm_columns WHERE view_name='earnings' ORDER BY 1,2")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>>>()?,
        vec![(0,"amount".into()),(0,"group_id".into()),(0,"join_key".into()),(1,"factor".into()),(1,"join_key".into())]);
    assert_eq!(db.prepare("SELECT object_type,COUNT(*) FROM __ivm_objects WHERE view_name='earnings' GROUP BY 1 ORDER BY 1")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>>>()?,
        vec![("index".into(),2),("table".into(),2),("trigger".into(),12)]);
    let schema = || -> Result<Vec<(String, String)>> {
        db.prepare("SELECT name,coalesce(sql,'') FROM main.sqlite_schema ORDER BY type,name")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    let before = schema()?;
    db.execute_batch("BEGIN")?;
    let dropped: String = db.query_row("SELECT sqlite_ivm_drop('EARNINGS')", [], |r| r.get(0))?;
    assert_eq!(dropped, "earnings");
    assert!(!db.is_autocommit());
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='earnings' OR name GLOB 'earnings_*' OR name GLOB '__ivm_earnings_*'", [], |r| r.get::<_, i64>(0))?, 0);
    db.execute_batch("UPDATE dimensions SET factor=30")?;
    verify(&db, "eligible_earnings", &eligible)?;
    assert_eq!(
        rows(&db, "SELECT * FROM eligible_earnings")?,
        vec![(4, 1, 240)]
    );
    db.execute_batch("ROLLBACK")?;
    assert_eq!(schema()?, before);
    verify(&db, "earnings", JOIN)?;
    verify(&db, "eligible_earnings", &eligible)?;
    db.execute_batch("UPDATE items SET amount=9 WHERE id=1")?;
    verify(&db, "earnings", JOIN)?;
    verify(&db, "eligible_earnings", &eligible)?;
    let source_before = rows(&db, "SELECT id,group_id,amount FROM items ORDER BY id")?;
    db.query_row("SELECT sqlite_ivm_drop('earnings')", [], |r| {
        r.get::<_, String>(0)
    })?;
    assert_eq!(
        rows(&db, "SELECT id,group_id,amount FROM items ORDER BY id")?,
        source_before
    );
    for table in [
        "__ivm_views",
        "__ivm_sources",
        "__ivm_columns",
        "__ivm_objects",
    ] {
        let column = if table == "__ivm_views" {
            "name"
        } else {
            "view_name"
        };
        assert_eq!(
            db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column}='earnings'"),
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
    }
    db.execute_batch("DELETE FROM items WHERE id=2; INSERT INTO items VALUES(3,10,9,10)")?;
    verify(&db, "eligible_earnings", &eligible)?;
    install(&db, "earnings", JOIN)?;
    verify(&db, "earnings", JOIN)?;
    Ok(())
}

#[test]
fn failed_drop_restores_objects_metadata_and_caller_transaction() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "INSERT INTO items VALUES(1,10,4,6); INSERT INTO dimensions VALUES(1,10,25,100)",
    )?;
    install(&db, "earnings", JOIN)?;
    let before = db
        .prepare("SELECT type,name,coalesce(sql,'') FROM sqlite_schema ORDER BY type,name")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    // Fail after all owned DDL has executed, during the final catalog deletion.
    db.execute_batch(
        "CREATE TEMP TRIGGER fail_drop BEFORE DELETE ON main.__ivm_views
        BEGIN SELECT RAISE(ABORT,'injected catalog failure'); END;
        BEGIN; UPDATE items SET amount=7",
    )?;
    // SQLite may report the numeric xDestroy failure without its zErrMsg text.
    assert!(db
        .query_row("SELECT sqlite_ivm_drop('earnings')", [], |r| r
            .get::<_, String>(0))
        .is_err());
    assert!(!db.is_autocommit());
    assert_eq!(
        db.prepare("SELECT type,name,coalesce(sql,'') FROM sqlite_schema ORDER BY type,name")?
            .query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            )))?
            .collect::<Result<Vec<_>>>()?,
        before
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM __ivm_objects WHERE view_name='earnings'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        16
    );
    verify(&db, "earnings", JOIN)?;
    assert_eq!(rows(&db, "SELECT * FROM earnings")?, vec![(4, 1, 175)]);
    db.execute_batch("DROP TRIGGER temp.fail_drop; UPDATE dimensions SET factor=30; COMMIT")?;
    verify(&db, "earnings", JOIN)?;
    db.execute_batch("SAVEPOINT caller")?;
    db.query_row("SELECT sqlite_ivm_drop('earnings')", [], |r| {
        r.get::<_, String>(0)
    })?;
    db.execute_batch("ROLLBACK TO caller; RELEASE caller; UPDATE items SET amount=8")?;
    verify(&db, "earnings", JOIN)?;
    Ok(())
}

#[test]
fn drop_uses_exact_catalog_ownership_and_rejects_missing_or_changed_objects() -> Result<()> {
    let db = database()?;
    assert!(db
        .query_row("SELECT sqlite_ivm_drop('items')", [], |r| r
            .get::<_, String>(0))
        .is_err());
    let name = "odd'\"_% view";
    let state = quote(&format!("{name}_state"));
    db.execute_batch(&format!(
        "CREATE TEMP TABLE {state}(marker INTEGER NOT NULL);
        INSERT INTO temp.{state} VALUES(42);
        CREATE TABLE main.{}(marker INTEGER NOT NULL);",
        quote(&format!("__ivm_{name}_unowned"))
    ))?;
    install(&db, name, JOIN)?;
    assert_eq!(
        db.query_row("SELECT sqlite_ivm_drop(?1)", [name], |r| r
            .get::<_, String>(0))?,
        name
    );
    assert_eq!(
        db.query_row(&format!("SELECT marker FROM temp.{state}"), [], |r| r
            .get::<_, i64>(0))?,
        42
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM main.sqlite_schema WHERE name=?1",
            [format!("__ivm_{name}_unowned")],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert!(db
        .query_row("SELECT sqlite_ivm_drop(?1)", [name], |r| r
            .get::<_, String>(0))
        .is_err());
    db.execute_batch(&format!("DROP TABLE temp.{state}"))?;
    install(&db, "earnings", JOIN)?;
    let definition: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='__ivm_earnings_key_0'",
        [],
        |r| r.get(0),
    )?;
    db.execute_batch("DROP INDEX __ivm_earnings_key_0;")?;
    for replacement in ["", "CREATE INDEX __ivm_earnings_key_0 ON items(amount)"] {
        db.execute_batch(replacement)?;
        assert!(db
            .query_row("SELECT sqlite_ivm_drop('earnings')", [], |r| r
                .get::<_, String>(0))
            .is_err());
        verify(&db, "earnings", JOIN)?;
    }
    db.execute_batch("DROP INDEX __ivm_earnings_key_0")?;
    db.execute_batch(&definition)?;
    db.query_row("SELECT sqlite_ivm_drop('earnings')", [], |r| {
        r.get::<_, String>(0)
    })?;
    Ok(())
}

#[test]
fn joins_maintain_both_sides_duplicates_moves_conflicts_and_rollback() -> Result<()> {
    let db = database()?;
    install(&db, "totals", JOIN)?;
    install(&db, "buckets", RIGHT_GROUP)?;
    let mutations = [
        "INSERT INTO items VALUES (1,10,4,7),(2,10,4,3),(3,20,9,-2),(4,30,12,0)",
        "INSERT INTO dimensions VALUES (1,10,2,100),(2,10,-1,101),(3,20,4,100)",
        "INSERT INTO dimensions VALUES (4,30,5,102)",
        "INSERT INTO items VALUES (5,10,8,6)",
        "UPDATE items SET amount=11 WHERE id=1",
        "UPDATE dimensions SET factor=3 WHERE id=2",
        "UPDATE items SET join_key=20,group_id=9,amount=7 WHERE id=1",
        "UPDATE dimensions SET join_key=10,bucket=103,factor=-2 WHERE id=3",
        "UPDATE items SET amount=amount+1 WHERE join_key=10",
        "UPDATE dimensions SET factor=factor+1 WHERE join_key=10",
        "INSERT INTO dimensions VALUES (5,10,3,101)",
        "INSERT OR IGNORE INTO items VALUES (2,99,99,999)",
        "UPDATE OR IGNORE items SET id=2 WHERE id=5",
        "INSERT INTO items VALUES (2,20,4,-7) ON CONFLICT(id) DO UPDATE SET amount=excluded.amount",
        "REPLACE INTO items VALUES (5,30,12,0)",
        "REPLACE INTO dimensions VALUES (3,20,-4,104)",
        "UPDATE OR REPLACE dimensions SET id=3 WHERE id=4",
        "DELETE FROM dimensions WHERE id=2",
        "DELETE FROM items WHERE group_id=4",
        "BEGIN; DELETE FROM dimensions",
        "ROLLBACK",
        "SAVEPOINT nested; UPDATE items SET group_id=99,amount=-9",
        "ROLLBACK TO nested; RELEASE nested",
        "DELETE FROM items",
        "DELETE FROM dimensions",
    ];
    for (index, mutation) in mutations.iter().enumerate() {
        db.execute_batch(mutation)?;
        verify(&db, "totals", JOIN)?;
        verify(&db, "buckets", RIGHT_GROUP)?;
        if index == 1 {
            assert_eq!(
                rows(&db, "SELECT * FROM totals ORDER BY 1")?,
                vec![(4, 4, 10), (9, 1, -8)]
            );
        }
        if index == 2 {
            assert_eq!(
                rows(&db, "SELECT * FROM totals ORDER BY 1")?,
                vec![(4, 4, 10), (9, 1, -8), (12, 1, 0)]
            );
        }
    }
    assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![]);
    Ok(())
}

#[test]
fn deterministic_mutations_match_original_join_after_every_statement() -> Result<()> {
    let db = database()?;
    let views = [
        ("totals", JOIN.to_string()),
        ("buckets", RIGHT_GROUP.to_string()),
        (
            "positive",
            JOIN.replace(" GROUP BY", " WHERE i.amount>0 AND d.factor<>0 GROUP BY"),
        ),
        (
            "either",
            JOIN.replace(" GROUP BY", " WHERE i.amount>0 OR d.factor<0 GROUP BY"),
        ),
        (
            "negated",
            RIGHT_GROUP.replace(
                " GROUP BY",
                " WHERE NOT (i.amount<=0 OR d.factor=0) GROUP BY",
            ),
        ),
        (
            "columns",
            JOIN.replace(
                " GROUP BY",
                " WHERE i.amount>d.factor AND d.bucket<=3 GROUP BY",
            ),
        ),
        (
            "composite",
            JOIN.replace(
                "ON i.join_key=d.join_key",
                "ON i.join_key=d.join_key AND d.bucket=i.group_id",
            ),
        ),
        (
            "composite_filtered",
            RIGHT_GROUP
                .replace(
                    "ON d.join_key=i.join_key",
                    "ON (d.join_key=i.join_key AND i.group_id=d.bucket)",
                )
                .replace(" GROUP BY", " WHERE i.amount>0 OR d.factor<0 GROUP BY"),
        ),
        (
            "three_keys",
            JOIN.replace(
                "ON i.join_key=d.join_key",
                "ON i.join_key=d.join_key AND i.group_id=d.bucket AND d.id=i.id",
            ),
        ),
    ];
    for (name, sql) in &views {
        install(&db, name, sql)?;
    }
    let mut seed = 42_u64;
    for step in 0..240 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let id = (seed >> 32) as i64 % 17;
        let key = (seed >> 40) as i64 % 5;
        let group = (seed >> 48) as i64 % 7;
        let value = (seed >> 56) as i64 % 19 - 9;
        match step % 6 {
            0 | 1 => {
                db.execute(
                    "REPLACE INTO items VALUES (?1,?2,?3,?4)",
                    [id, key, group, value],
                )?;
            }
            2 => {
                db.execute(
                    "REPLACE INTO dimensions VALUES (?1,?2,?3,?4)",
                    [id, key, value, group],
                )?;
            }
            3 => {
                db.execute("DELETE FROM items WHERE id=?1", [id])?;
            }
            4 => {
                db.execute(
                    "UPDATE dimensions SET join_key=?1,factor=?2,bucket=?3 WHERE id=?4",
                    [key, value, group, id],
                )?;
            }
            _ => {
                db.execute("DELETE FROM dimensions WHERE id=?1", [id])?;
            }
        }
        for (name, sql) in &views {
            verify(&db, name, sql)?;
        }
    }
    Ok(())
}

#[test]
fn unrelated_groups_are_never_written_and_join_keys_are_indexed() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "INSERT INTO items VALUES (1,10,4,7),(2,10,4,3);
        INSERT INTO dimensions VALUES (1,10,2,100);
        WITH RECURSIVE seq(x) AS (VALUES(100) UNION ALL SELECT x+1 FROM seq WHERE x<1099)
        INSERT INTO items SELECT x,x,x,x FROM seq;
        INSERT INTO dimensions SELECT id,join_key,1,group_id FROM items WHERE id>=100;",
    )?;
    install(&db, "totals", JOIN)?;
    db.execute_batch(
        "CREATE TABLE writes(event TEXT, g INTEGER NOT NULL);
        CREATE TRIGGER observe_insert AFTER INSERT ON totals_state BEGIN
            INSERT INTO writes VALUES ('insert',NEW.g); END;
        CREATE TRIGGER observe_update AFTER UPDATE ON totals_state BEGIN
            INSERT INTO writes VALUES ('update',NEW.g); END;
        CREATE TRIGGER observe_delete AFTER DELETE ON totals_state BEGIN
            INSERT INTO writes VALUES ('delete',OLD.g); END;
        UPDATE items SET amount=9 WHERE id=1;",
    )?;
    let writes = db
        .prepare("SELECT event,g FROM writes ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(writes, vec![("update".into(), 4), ("update".into(), 4)]);
    verify(&db, "totals", JOIN)?;
    for table in ["items", "dimensions"] {
        let details = db
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT * FROM {table} WHERE join_key=10"
            ))?
            .query_map([], |r| r.get::<_, String>(3))?
            .collect::<Result<Vec<_>>>()?;
        assert!(
            details
                .iter()
                .any(|s| s.contains("SEARCH") && s.contains("__ivm_totals_key_")),
            "{details:?}"
        );
    }
    Ok(())
}

#[test]
fn rejected_values_overflow_and_writer_settings_preserve_state() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "INSERT INTO items VALUES (1,10,4,7),(2,10,4,3);
        INSERT INTO dimensions VALUES (1,10,2,100)",
    )?;
    install(&db, "totals", JOIN)?;
    let before = rows(&db, "SELECT * FROM totals")?;
    for sql in [
        "INSERT INTO items VALUES (3,10,4,NULL)",
        "INSERT OR IGNORE INTO items VALUES (3,10,4,'text')",
        "INSERT INTO items VALUES (3,10,4,1.5)",
        "UPDATE dimensions SET factor=9223372036854775807",
        "INSERT INTO items VALUES (3,10,4,9223372036854775807)",
        "UPDATE items SET group_id=NULL",
    ] {
        assert!(db.execute_batch(sql).is_err(), "{sql}");
        assert_eq!(rows(&db, "SELECT * FROM totals")?, before, "{sql}");
        verify(&db, "totals", JOIN)?;
    }
    db.execute_batch("PRAGMA recursive_triggers=OFF")?;
    assert!(db
        .execute_batch("REPLACE INTO items VALUES (1,10,4,1)")
        .is_err());
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=OFF")?;
    assert!(db.execute_batch("DELETE FROM items").is_err());
    db.execute_batch("PRAGMA trusted_schema=ON")?;
    verify(&db, "totals", JOIN)?;
    assert_eq!(rows(&db, "SELECT * FROM totals")?, before);
    Ok(())
}

#[test]
fn install_failure_rolls_back_created_objects_and_preserves_caller_transaction() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "BEGIN; INSERT INTO items VALUES (1,10,4,7);
        CREATE INDEX __ivm_bad_key_1 ON dimensions(factor);",
    )?;
    assert!(install(&db, "bad", JOIN).is_err());
    assert!(!db.is_autocommit());
    let names = db
        .prepare("SELECT name FROM sqlite_schema WHERE name LIKE '__ivm_bad_%' ORDER BY name")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(names, vec!["__ivm_bad_key_1"]);
    install(&db, "totals", JOIN)?;
    assert!(!db.is_autocommit());
    db.execute_batch("INSERT INTO dimensions VALUES (1,10,2,100)")?;
    assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![(4, 1, 14)]);
    db.execute_batch("ROLLBACK")?;
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='totals'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='__ivm_views'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    install(&db, "totals", JOIN)?;
    db.execute_batch(
        "CREATE TEMP TRIGGER reject_manifest BEFORE INSERT ON main.__ivm_objects
        WHEN NEW.view_name='blocked' BEGIN SELECT RAISE(ABORT,'injected manifest failure'); END;",
    )?;
    assert!(install(&db, "blocked", JOIN).is_err());
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='blocked' OR name GLOB 'blocked_*' OR name GLOB '__ivm_blocked_*'", [], |r| r.get::<_, i64>(0))?, 0);
    for table in [
        "__ivm_views",
        "__ivm_sources",
        "__ivm_columns",
        "__ivm_objects",
    ] {
        let column = if table == "__ivm_views" {
            "name"
        } else {
            "view_name"
        };
        assert_eq!(
            db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column}='blocked'"),
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
    }
    verify(&db, "totals", JOIN)?;
    Ok(())
}

#[test]
fn single_table_integer_boundaries_and_public_view_are_enforced() -> Result<()> {
    let db = database()?;
    let sql = "SELECT group_id AS g, COUNT(*) AS n, SUM(amount) AS s FROM items GROUP BY group_id";
    install(&db, "totals", sql)?;
    for mutation in [
        "INSERT INTO items VALUES (1,10,4,-9223372036854775808)",
        "DELETE FROM items",
        "INSERT INTO items VALUES (1,10,4,9223372036854775807)",
    ] {
        db.execute_batch(mutation)?;
        verify(&db, "totals", sql)?;
    }
    assert!(db
        .execute_batch("INSERT INTO items VALUES (2,10,4,1)")
        .is_err());
    assert!(db.execute_batch("UPDATE totals SET s=0").is_err());
    verify(&db, "totals", sql)?;
    Ok(())
}

#[test]
fn filters_cover_all_four_update_transitions_from_both_join_sides() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "INSERT INTO items VALUES (1,10,4,7),(2,20,9,-3);
        INSERT INTO dimensions VALUES (1,10,2,100),(2,20,0,101)",
    )?;
    let sql = JOIN.replace(" GROUP BY", " WHERE i.amount>0 AND d.factor<>0 GROUP BY");
    install(&db, "totals", &sql)?;
    assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![(4, 1, 14)]);
    db.execute_batch(
        "CREATE TABLE writes(g INTEGER NOT NULL);
        CREATE TRIGGER watch_insert AFTER INSERT ON totals_state BEGIN
            INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER watch_update AFTER UPDATE ON totals_state BEGIN
            INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER watch_delete AFTER DELETE ON totals_state BEGIN
            INSERT INTO writes VALUES(OLD.g); END;",
    )?;
    let steps = [
        (
            "left true -> true",
            "UPDATE items SET amount=8 WHERE id=1",
            vec![(4, 1, 16)],
            false,
        ),
        (
            "left false -> false",
            "UPDATE items SET amount=-4 WHERE id=2",
            vec![(4, 1, 16)],
            true,
        ),
        (
            "left true -> false",
            "UPDATE items SET amount=-1 WHERE id=1",
            vec![],
            false,
        ),
        (
            "left false -> true",
            "UPDATE items SET amount=5 WHERE id=1",
            vec![(4, 1, 10)],
            false,
        ),
        (
            "right true -> true",
            "UPDATE dimensions SET factor=3 WHERE id=1",
            vec![(4, 1, 15)],
            false,
        ),
        (
            "right true -> false",
            "UPDATE dimensions SET factor=0 WHERE id=1",
            vec![],
            false,
        ),
        (
            "right false -> false",
            "UPDATE dimensions SET bucket=105 WHERE id=1",
            vec![],
            true,
        ),
        (
            "right false -> true",
            "UPDATE dimensions SET factor=-2 WHERE id=1",
            vec![(4, 1, -10)],
            false,
        ),
        (
            "left key + group + filter exit",
            "UPDATE items SET join_key=20,group_id=9,amount=-5 WHERE id=1",
            vec![],
            false,
        ),
        (
            "right key + filter entry without matches",
            "UPDATE dimensions SET join_key=10,factor=4 WHERE id=2",
            vec![],
            true,
        ),
        (
            "left key + group + filter entry",
            "UPDATE items SET join_key=10,group_id=8,amount=6 WHERE id=1",
            vec![(8, 2, 12)],
            false,
        ),
        (
            "right key + filter exit",
            "UPDATE dimensions SET join_key=20,factor=0 WHERE id=2",
            vec![(8, 1, -12)],
            false,
        ),
        (
            "filtered insert",
            "INSERT INTO items VALUES(3,10,8,-9)",
            vec![(8, 1, -12)],
            true,
        ),
        (
            "filtered delete",
            "DELETE FROM items WHERE id=3",
            vec![(8, 1, -12)],
            true,
        ),
        (
            "last included row deleted",
            "DELETE FROM items WHERE id=1",
            vec![],
            false,
        ),
    ];
    for (label, mutation, expected, no_writes) in steps {
        db.execute_batch("DELETE FROM writes")?;
        db.execute_batch(mutation)?;
        verify(&db, "totals", &sql)?;
        assert_eq!(
            rows(&db, "SELECT * FROM totals ORDER BY 1")?,
            expected,
            "{label}"
        );
        if no_writes {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM writes", [], |r| r.get::<_, i64>(0))?,
                0,
                "{label}"
            );
        }
    }
    Ok(())
}

#[test]
fn single_table_filters_boolean_precedence_integer_limits_and_rollback() -> Result<()> {
    let db = database()?;
    let filters = [
        "amount > 0",
        "NOT (amount <= 0 OR group_id = 9)",
        "(amount = -5 OR amount >= +7) AND amount <= 11",
        "amount >= -9223372036854775808 AND amount < 9223372036854775807",
        "0 < amount AND amount <> group_id",
        "1 = 0",
    ];
    let views = filters.iter().enumerate().map(|(i, filter)| (format!("v{i}"), format!(
        "SELECT group_id AS g, COUNT(*) AS n, SUM(amount) AS s FROM items WHERE {filter} GROUP BY group_id"
    ))).collect::<Vec<_>>();
    db.execute_batch("INSERT INTO items VALUES (1,10,4,-5),(2,10,4,7),(3,20,9,0)")?;
    for (name, sql) in &views {
        install(&db, name, sql)?;
    }
    for mutation in [
        "UPDATE items SET amount=11 WHERE id=1",
        "UPDATE items SET amount=-5,group_id=9 WHERE id=2",
        "INSERT INTO items VALUES (4,20,9,4)",
        "BEGIN; UPDATE items SET amount=-amount",
        "ROLLBACK",
        "SAVEPOINT f; DELETE FROM items WHERE amount>0",
        "ROLLBACK TO f; RELEASE f",
        "DELETE FROM items",
        "INSERT INTO items VALUES (1,10,4,-9223372036854775808)",
        "DELETE FROM items",
        "INSERT INTO items VALUES (1,10,4,9223372036854775807)",
        "DELETE FROM items",
    ] {
        db.execute_batch(mutation)?;
        for (name, sql) in &views {
            verify(&db, name, sql)?;
        }
    }
    Ok(())
}

#[test]
fn columns_used_only_by_filters_are_checked_at_install_and_on_writes() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "ALTER TABLE items ADD COLUMN enabled INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE dimensions ADD COLUMN enabled INTEGER NOT NULL DEFAULT 0;
        INSERT INTO items VALUES(1,10,4,7,'invalid');
        INSERT INTO dimensions VALUES(1,10,2,100,1)",
    )?;
    let sql = JOIN.replace(" GROUP BY", " WHERE i.enabled=1 AND d.enabled=1 GROUP BY");
    assert!(install(&db, "totals", &sql).is_err());
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE '__ivm_totals_%'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    db.execute_batch("UPDATE items SET enabled=1")?;
    install(&db, "totals", &sql)?;
    assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![(4, 1, 14)]);
    for statement in [
        "UPDATE items SET enabled=NULL",
        "UPDATE OR IGNORE dimensions SET enabled='invalid'",
        "UPDATE dimensions SET enabled=0.5",
    ] {
        assert!(db.execute_batch(statement).is_err(), "{statement}");
        verify(&db, "totals", &sql)?;
        assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![(4, 1, 14)]);
    }
    db.execute_batch("UPDATE dimensions SET enabled=0")?;
    verify(&db, "totals", &sql)?;
    assert_eq!(rows(&db, "SELECT * FROM totals")?, vec![]);
    Ok(())
}

#[test]
fn composite_keys_isolate_partial_matches_and_maintain_moves_on_both_sides() -> Result<()> {
    let db = database()?;
    db.execute_batch(
        "ALTER TABLE items ADD COLUMN farm_id INTEGER NOT NULL DEFAULT 1;
        ALTER TABLE dimensions ADD COLUMN farm_id INTEGER NOT NULL DEFAULT 1;
        INSERT INTO items VALUES (1,10,4,6,1),(2,10,4,8,2),(3,20,9,10,1);
        INSERT INTO dimensions VALUES (1,10,25,100,1),(2,10,30,101,2),
            (3,20,8,102,1),(4,10,5,103,1)",
    )?;
    let sql = JOIN.replace(
        "ON i.join_key=d.join_key",
        "ON (i.farm_id=d.farm_id AND d.join_key=i.join_key)",
    );
    let right = RIGHT_GROUP.replace(
        "ON d.join_key=i.join_key",
        "ON d.farm_id=i.farm_id AND (d.join_key=i.join_key)",
    );
    let filtered = sql.replace(" GROUP BY", " WHERE i.amount>=5 AND d.factor>0 GROUP BY");
    for (name, query) in [
        ("totals", &sql),
        ("buckets", &right),
        ("eligible", &filtered),
    ] {
        install(&db, name, query)?;
        verify(&db, name, query)?;
    }
    assert_eq!(
        rows(&db, "SELECT * FROM totals ORDER BY g")?,
        vec![(4, 3, 420), (9, 1, 80)]
    );
    for table in ["items", "dimensions"] {
        let plan = db
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT * FROM {table} WHERE farm_id=1 AND join_key=10"
            ))?
            .query_map([], |r| r.get::<_, String>(3))?
            .collect::<Result<Vec<_>>>()?;
        assert!(
            plan.iter()
                .any(|s| s.contains("SEARCH") && s.contains("farm_id=? AND join_key=?")),
            "{plan:?}"
        );
    }
    db.execute_batch("CREATE TABLE writes(g INTEGER NOT NULL);
        CREATE TRIGGER watch_insert AFTER INSERT ON totals_state BEGIN INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER watch_update AFTER UPDATE ON totals_state BEGIN INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER watch_delete AFTER DELETE ON totals_state BEGIN INSERT INTO writes VALUES(OLD.g); END;
        UPDATE dimensions SET factor=26 WHERE id=1;")?;
    assert_eq!(
        db.prepare("SELECT DISTINCT g FROM writes ORDER BY g")?
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>>>()?,
        vec![4]
    );
    for mutation in [
        "INSERT INTO items VALUES(4,10,7,9,3)", // Only crop matches; no price for farm 3.
        "INSERT INTO dimensions VALUES(5,20,7,104,3)", // Only farm matches the new lot.
        "UPDATE items SET farm_id=2 WHERE id=1", // Change the first key component.
        "UPDATE dimensions SET farm_id=2 WHERE id=4",
        "UPDATE items SET join_key=20 WHERE id=1", // Change the second key component.
        "UPDATE dimensions SET join_key=20 WHERE id=4",
        "UPDATE items SET farm_id=1,join_key=10,group_id=8,amount=3 WHERE id=1",
        "UPDATE dimensions SET farm_id=3,join_key=10,factor=-2,bucket=105 WHERE id=4",
        "UPDATE items SET amount=7 WHERE id=1", // Re-enter the filter.
        "UPDATE dimensions SET factor=4 WHERE id=4",
        "INSERT INTO dimensions VALUES(6,10,2,106,1)", // Duplicate complete key.
        "UPDATE dimensions SET factor=factor+1 WHERE farm_id=1",
        "UPDATE items SET farm_id=1 WHERE farm_id=2",
        "REPLACE INTO items VALUES(1,20,8,5,3)",
        "INSERT INTO dimensions VALUES(6,20,9,106,3) ON CONFLICT(id) DO UPDATE SET farm_id=excluded.farm_id,join_key=excluded.join_key",
        "BEGIN; UPDATE dimensions SET farm_id=99",
        "ROLLBACK",
        "SAVEPOINT move; UPDATE items SET farm_id=99,join_key=99",
        "ROLLBACK TO move; RELEASE move",
        "DELETE FROM dimensions WHERE id=4",
        "DELETE FROM items WHERE id=1",
        "DELETE FROM dimensions",
        "DELETE FROM items",
    ] {
        db.execute_batch(mutation)?;
        for (name, query) in [("totals", &sql), ("buckets", &right), ("eligible", &filtered)] {
            verify(&db, name, query)?;
        }
    }
    for statement in [
        "INSERT INTO items VALUES(1,10,4,6,NULL)",
        "INSERT INTO dimensions VALUES(1,10,25,100,'bad')",
        "INSERT OR IGNORE INTO dimensions VALUES(1,10,25,100,1.5)",
    ] {
        assert!(db.execute_batch(statement).is_err(), "{statement}");
        verify(&db, "totals", &sql)?;
    }
    db.execute_batch(
        "DROP TRIGGER watch_insert; DROP TRIGGER watch_update; DROP TRIGGER watch_delete;",
    )?;
    // Key-only columns receive the same checks when initially binding populated sources.
    let invalid = database()?;
    invalid.execute_batch(
        "ALTER TABLE items ADD COLUMN farm_id INTEGER NOT NULL;
        ALTER TABLE dimensions ADD COLUMN farm_id INTEGER NOT NULL;
        INSERT INTO items VALUES(1,10,4,6,'bad')",
    )?;
    assert!(install(&invalid, "bad", &sql).is_err());
    assert_eq!(
        invalid.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE '__ivm_bad_%'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}
