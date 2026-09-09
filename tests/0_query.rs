#![cfg(not(feature = "extension"))]

use rusqlite::{Connection, Result};
use sqlite_ivm::query::{bind, quote, Column, Query};

fn col(name: &str) -> Column {
    Column {
        source: 0,
        name: name.into(),
    }
}

fn database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        r#"
        CREATE TABLE items (id INTEGER PRIMARY KEY, group_id INTEGER, amount INTEGER);
        INSERT INTO items VALUES (1,4,7), (2,4,3), (3,9,-2), (4,12,0);
        CREATE TABLE "odd"" table" ("g] key" INTEGER, "sum value" INTEGER);
        CREATE TABLE text_items (group_id INTEGER, amount TEXT);
        CREATE TABLE generated_items (
            group_id INTEGER, amount INTEGER,
            derived INTEGER GENERATED ALWAYS AS (amount + 1) STORED
        );
        CREATE TABLE __ivm_internal (group_id INTEGER, amount INTEGER);
        CREATE VIEW item_view AS SELECT * FROM items;
        ATTACH ':memory:' AS other;
        CREATE TABLE other.items (group_id INTEGER, amount INTEGER);
    "#,
    )?;
    Ok(db)
}

#[test]
fn accepted_queries_bind_catalog_names_and_output_order() -> Result<()> {
    let db = database()?;
    let cases = [
        ("SELECT group_id, COUNT(*) AS item_count, SUM(amount) AS total_amount FROM items GROUP BY group_id",
         vec![("group_id", "g"), ("item_count", "n"), ("total_amount", "s")]),
        ("select GROUP_ID, count(*), sum(AMOUNT) from ITEMS group by GROUP_ID; -- comment",
         vec![("group_id", "g"), ("count(*)", "n"), ("sum(AMOUNT)", "s")]),
        ("SELECT i.group_id AS g, SUM(i.amount) AS s, COUNT(*) AS n FROM main.items AS i GROUP BY i.group_id",
         vec![("g", "g"), ("s", "s"), ("n", "n")]),
        ("SELECT COUNT(*) AS n, i.group_id AS g, SUM(i.amount) AS s FROM items i GROUP BY i.group_id",
         vec![("n", "n"), ("g", "g"), ("s", "s")]),
        ("SELECT [group_id], COUNT(*) AS [count value], SUM([amount]) AS `total value` FROM [items] GROUP BY [group_id]",
         vec![("group_id", "g"), ("count value", "n"), ("total value", "s")]),
    ];
    for (sql, outputs) in cases {
        assert_eq!(
            bind(&db, sql)?,
            Query {
                tables: vec!["items".into()],
                join: None,
                group: col("group_id"),
                factors: vec![col("amount")],
                filter: None,
                outputs: outputs
                    .into_iter()
                    .map(|(name, kind)| (name.into(), kind))
                    .collect(),
            },
            "{sql}"
        );
    }
    assert_eq!(
        bind(
            &db,
            r#"SELECT "g] key", COUNT(*) AS "n", SUM("sum value") AS "s"
        FROM "odd"" table" GROUP BY "g] key";"#
        )?,
        Query {
            tables: vec!["odd\" table".into()],
            join: None,
            group: col("g] key"),
            factors: vec![col("sum value")],
            filter: None,
            outputs: vec![("g] key".into(), "g"), ("n".into(), "n"), ("s".into(), "s")],
        }
    );
    Ok(())
}

#[test]
fn binding_is_read_only_and_matches_sqlite_results_through_crud() -> Result<()> {
    let db = database()?;
    let sql = "SELECT group_id, COUNT(*) AS n, SUM(amount) AS s FROM items GROUP BY group_id";
    let steps = [
        ("", vec![(4, 2, 10), (9, 1, -2), (12, 1, 0)]),
        (
            "INSERT INTO items VALUES (5,4,6)",
            vec![(4, 3, 16), (9, 1, -2), (12, 1, 0)],
        ),
        (
            "UPDATE items SET group_id=9, amount=11 WHERE id=1",
            vec![(4, 2, 9), (9, 2, 9), (12, 1, 0)],
        ),
        (
            "DELETE FROM items WHERE group_id=4",
            vec![(9, 2, 9), (12, 1, 0)],
        ),
        ("DELETE FROM items", vec![]),
    ];
    for (mutation, expected) in steps {
        db.execute_batch(mutation)?;
        let before = db.total_changes();
        let plan = bind(&db, sql)?;
        assert_eq!(db.total_changes(), before, "binding changed rows");
        let rebound = format!(
            "SELECT {}, COUNT(*), SUM({}) FROM main.{} GROUP BY {} ORDER BY 1",
            quote(&plan.group.name),
            quote(&plan.factors[0].name),
            quote(&plan.tables[0]),
            quote(&plan.group.name)
        );
        for query in [format!("{sql} ORDER BY 1"), rebound] {
            let rows = db
                .prepare(&query)?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(rows, expected, "after {mutation}: {query}");
        }
    }
    Ok(())
}

#[test]
fn unsupported_queries_are_rejected_without_changing_schema_or_rows() -> Result<()> {
    let db = database()?;
    let cases = [
        "",
        "SELEC group_id FROM items",
        "DELETE FROM items",
        "EXPLAIN SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id; DELETE FROM items",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id; garbage",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items WHERE amount LIKE '1%' GROUP BY group_id",
        "SELECT DISTINCT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id HAVING COUNT(*)>0",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id ORDER BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id LIMIT 1",
        "WITH x AS (SELECT * FROM items) SELECT group_id, COUNT(*), SUM(amount) FROM x GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id UNION SELECT 1,2,3",
        "SELECT i.group_id, COUNT(*), SUM(i.amount) FROM items i JOIN items j ON i.id=j.id GROUP BY i.group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM (SELECT * FROM items) GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM item_view GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM missing GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM other.items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM __ivm_internal GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM text_items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(derived) FROM generated_items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(missing) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount * 2) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(amount), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), AVG(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(DISTINCT amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*) FILTER (WHERE amount>0), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*) OVER (), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id, amount",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items",
        "SELECT group_id AS x, COUNT(*) AS X, SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), COUNT(*) FROM items GROUP BY group_id",
        "SELECT group_id, SUM(amount), SUM(amount) FROM items GROUP BY group_id",
        "SELECT *, COUNT(*), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount), 1 FROM items GROUP BY group_id",
        "SELECT 1, COUNT(*), SUM(amount) FROM items GROUP BY group_id",
        "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY 1",
    ];
    let schema_before: String = db.query_row(
        "SELECT group_concat(sql, char(10)) FROM main.sqlite_schema",
        [],
        |r| r.get(0),
    )?;
    let changes_before = db.total_changes();
    for sql in cases {
        assert!(bind(&db, sql).is_err(), "unexpected acceptance: {sql}");
        assert_eq!(db.total_changes(), changes_before, "{sql}");
        let schema_after: String = db.query_row(
            "SELECT group_concat(sql, char(10)) FROM main.sqlite_schema",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(schema_after, schema_before, "{sql}");
    }
    assert!(bind(&db, &" ".repeat(65_537)).is_err());
    assert!(bind(&db, "SELECT\0 1").is_err());
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0))?,
        4
    );
    Ok(())
}

#[test]
fn temporary_shadowing_is_rejected() -> Result<()> {
    let db = database()?;
    db.execute_batch("CREATE TEMP TABLE items (group_id INTEGER, amount INTEGER)")?;
    assert_eq!(
        bind(
            &db,
            "SELECT group_id, COUNT(*), SUM(amount) FROM items GROUP BY group_id"
        )
        .unwrap_err()
        .to_string(),
        "a temporary object shadows the source table"
    );
    Ok(())
}

#[test]
fn unsupported_filter_forms_are_rejected_before_mutation() -> Result<()> {
    let db = database()?;
    let before = db.total_changes();
    for filter in [
        "amount > 0.5",
        "amount = '7'",
        "amount IS NULL",
        "amount IN (1,2)",
        "abs(amount) > 0",
        "amount + 1 > 0",
        "amount > ?1",
        "amount > missing",
        "amount > 9223372036854775808",
        "amount < -9223372036854775809",
        "amount > 0x10",
        "amount > 1e2",
        "amount > 1_000",
        "amount > (SELECT 1)",
        "amount BETWEEN 1 AND 9",
        "amount",
        "NULL",
    ] {
        let sql = format!(
            "SELECT group_id,COUNT(*),SUM(amount) FROM items WHERE {filter} GROUP BY group_id"
        );
        assert!(bind(&db, &sql).is_err(), "unexpected acceptance: {filter}");
        assert_eq!(db.total_changes(), before);
    }
    let nested = format!("{}amount>0{}", "(".repeat(65), ")".repeat(65));
    let sql =
        format!("SELECT group_id,COUNT(*),SUM(amount) FROM items WHERE {nested} GROUP BY group_id");
    assert!(bind(&db, &sql).is_err());
    Ok(())
}

#[test]
fn composite_join_binding_preserves_pairs_and_rejects_other_predicates() -> Result<()> {
    let db = database()?;
    db.execute_batch("CREATE TABLE prices(farm_id INTEGER, crop_id INTEGER, price INTEGER)")?;
    let query = |on: &str| {
        format!(
            "SELECT i.group_id,COUNT(*),SUM(i.amount*p.price)
        FROM items i JOIN prices p ON {on} GROUP BY i.group_id"
        )
    };
    let before = db.total_changes();
    for on in [
        "i.group_id=p.farm_id AND i.id=p.crop_id",
        "(p.farm_id=i.group_id) AND ((i.id=p.crop_id))",
        "i.group_id=p.farm_id AND (i.id=p.crop_id AND p.farm_id=i.group_id)",
    ] {
        assert_eq!(
            bind(&db, &query(on))?.join,
            Some(vec![
                [
                    col("group_id"),
                    Column {
                        source: 1,
                        name: "farm_id".into()
                    }
                ],
                [
                    col("id"),
                    Column {
                        source: 1,
                        name: "crop_id".into()
                    }
                ],
            ])
        );
    }
    for on in [
        "i.group_id=p.farm_id OR i.id=p.crop_id",
        "i.group_id=p.farm_id AND i.id>p.crop_id",
        "i.group_id=p.farm_id AND i.id=i.amount",
        "i.group_id=p.farm_id AND p.price=1",
        "i.group_id=p.farm_id AND i.id+1=p.crop_id",
        "i.group_id=p.farm_id AND i.id=p.missing",
        "NOT (i.group_id=p.farm_id)",
    ] {
        assert!(bind(&db, &query(on)).is_err(), "{on}");
    }
    let nested = format!("{}i.id=p.crop_id{}", "(".repeat(65), ")".repeat(65));
    assert!(bind(&db, &query(&nested)).is_err());
    assert_eq!(db.total_changes(), before);
    Ok(())
}
