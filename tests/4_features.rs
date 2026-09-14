#![cfg(not(feature = "extension"))]
use rusqlite::{types::Value, Connection, Result};
#[path = "support/0_database.rs"]
mod database;
use database::register;

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut s = db.prepare(sql)?;
    let n = s.column_count();
    let mut rows = s
        .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
        .collect::<Result<Vec<Vec<Value>>>>()?;
    rows.sort_by_key(|r| format!("{r:?}"));
    Ok(rows)
}

#[test]
fn feature_compositions_against_sqlite() -> Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/1_features.json")).unwrap();
    let mut failures = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let query = case["query"].as_str().unwrap();
        let result = (|| -> Result<()> {
            let db = Connection::open_in_memory()?;
            register(&db)?;
            db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
            db.execute_batch(fixture["schema"].as_str().unwrap())?;
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
                query.replace('\'', "''")
            ))?;
            assert_eq!(
                rows(&db, "SELECT * FROM result")?,
                rows(&db, query)?,
                "{name}: empty"
            );
            for (step, sql) in fixture["mutations"].as_array().unwrap().iter().enumerate() {
                let sql = sql.as_str().unwrap();
                db.execute_batch(sql)
                    .unwrap_or_else(|e| panic!("{name}: {step}: {sql}: {e}"));
                assert_eq!(
                    rows(&db, "SELECT * FROM result")?,
                    rows(&db, query)?,
                    "{name}: {step}: {sql}"
                );
            }
            Ok(())
        })();
        if let Err(e) = result {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn blobs_and_adjacent_floats_survive_trigger_transport() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k BLOB,v REAL);CREATE TABLE b(k BLOB);
        CREATE VIRTUAL TABLE values_view USING sqlite_ivm('SELECT id,k,v,typeof(k) AS kind FROM a');
        CREATE VIRTUAL TABLE joined USING sqlite_ivm('SELECT a.id,a.k FROM a JOIN b ON a.k=b.k');
        CREATE VIRTUAL TABLE distinct_values USING sqlite_ivm('SELECT DISTINCT v FROM a');
        CREATE VIRTUAL TABLE bounds USING sqlite_ivm('SELECT k,MIN(v) AS lo,MAX(v) AS hi FROM a GROUP BY k')")?;
    let queries = [
        ("values_view", "SELECT id,k,v,typeof(k) FROM a"),
        ("joined", "SELECT a.id,a.k FROM a JOIN b ON a.k=b.k"),
        ("distinct_values", "SELECT DISTINCT v FROM a"),
        ("bounds", "SELECT k,MIN(v),MAX(v) FROM a GROUP BY k"),
    ];
    let verify = || -> Result<()> {
        for (view, query) in queries {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM {view}"))?,
                rows(&db, query)?,
                "{view}"
            );
        }
        Ok(())
    };
    for (id, key, value) in [
        (1, vec![], 1.0000000000000002f64),
        (2, vec![0, 255, 0], 1.0000000000000004),
        (3, vec![0, 255, 0], 1.2345678901234567),
        (4, vec![0, 255, 0], 1.2345678901234567),
        (5, vec![], f64::INFINITY),
        (6, vec![], f64::NEG_INFINITY),
        (7, vec![], f64::from_bits(1)),
    ] {
        db.execute(
            "INSERT INTO a VALUES(?1,?2,?3)",
            rusqlite::params![id, key, value],
        )?;
        verify()?;
        db.execute("INSERT INTO b VALUES(?1)", [&key])?;
        verify()?;
    }
    for sql in [
        "BEGIN",
        "UPDATE a SET k=x'00',v=1.0000000000000002 WHERE id=3",
        "DELETE FROM a WHERE id=2",
        "ROLLBACK",
        "DELETE FROM a WHERE id=3",
        "DELETE FROM b WHERE k=x'00ff00'",
        "DELETE FROM a",
    ] {
        db.execute_batch(sql)?;
        verify()?;
    }
    Ok(())
}

#[test]
fn collations_control_group_distinct_join_and_outer_predicates() -> Result<()> {
    for collation in ["NOCASE", "RTRIM"] {
        let db = Connection::open_in_memory()?;
        register(&db)?;
        db.execute_batch(&format!("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k TEXT COLLATE {collation},v INTEGER);CREATE TABLE b(bid INTEGER PRIMARY KEY,k TEXT COLLATE {collation})"))?;
        let normalize = if collation == "NOCASE" {
            "lower"
        } else {
            "rtrim"
        };
        let queries=[format!("SELECT {normalize}(k) AS k,COUNT(*) AS n,SUM(v) AS s,COUNT(DISTINCT k) AS keys FROM a GROUP BY k"),
            format!("SELECT {normalize}(k) AS k FROM (SELECT DISTINCT k FROM a)"),
            "SELECT a.id,b.bid FROM a FULL JOIN b ON a.k=b.k".into(),
            "SELECT a.id,b.bid FROM a FULL JOIN b USING(k)".into(),
            "SELECT id FROM a WHERE k='alpha' COLLATE BINARY".into(),
            "SELECT k FROM a".into()];
        for (i, q) in queries.iter().enumerate() {
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{}')",
                q.replace('\'', "''")
            ))?;
        }
        for sql in [
            "INSERT INTO a VALUES(1,'alpha',7),(2,'ALPHA',9),(3,'alpha ',4),(4,NULL,2)",
            "INSERT INTO b VALUES(1,'alpha'),(2,'ALPHA'),(3,'alpha '),(4,NULL)",
            "UPDATE a SET k='ALPHA' WHERE id=3",
            "DELETE FROM b WHERE bid=1",
            "BEGIN;DELETE FROM a WHERE id=1",
            "ROLLBACK",
            "DELETE FROM a WHERE id=1",
            "UPDATE b SET k='other'",
            "DELETE FROM a",
            "DELETE FROM b",
        ] {
            db.execute_batch(sql)?;
            for (i, q) in queries.iter().enumerate() {
                assert_eq!(
                    rows(&db, &format!("SELECT * FROM v{i}"))?,
                    rows(&db, q)?,
                    "{collation}: {sql}: {q}"
                );
            }
            assert_eq!(
                rows(&db, "SELECT k FROM v5 WHERE k='alpha'")?,
                rows(&db, "SELECT k FROM a WHERE k='alpha'")?
            );
        }
    }
    Ok(())
}

#[test]
fn recursive_delete_preserves_alternate_null_support_and_collated_roots() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE roots(k TEXT COLLATE NOCASE);CREATE TABLE edges(a TEXT COLLATE NOCASE,b TEXT COLLATE NOCASE)")?;
    let query="WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM edges e JOIN r ON e.a=r.n) SELECT lower(n) AS n FROM r";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE reached USING sqlite_ivm('{query}')"
    ))?;
    for sql in ["INSERT INTO roots VALUES('a'),('A'),('x')","INSERT INTO edges VALUES('A','b'),('B','c'),('c','a'),('B',NULL),('x',NULL),(NULL,'unreachable')","DELETE FROM edges WHERE a='b' AND b IS NULL","DELETE FROM roots WHERE k='a' COLLATE BINARY","DELETE FROM roots WHERE k='A' COLLATE BINARY","BEGIN;DELETE FROM edges WHERE a='x'","ROLLBACK","INSERT INTO roots VALUES(NULL)","DELETE FROM edges WHERE a='x'","DELETE FROM roots","INSERT INTO roots VALUES('A')","DELETE FROM edges WHERE a='b'","DELETE FROM edges","DELETE FROM roots"]{
        db.execute_batch(sql)?;assert_eq!(rows(&db,"SELECT * FROM reached")?,rows(&db,query)?,"{sql}");
    }
    Ok(())
}

#[test]
fn parenthesized_join_scopes_and_nonrecursive_union_cte() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER);CREATE TABLE b(bid INTEGER PRIMARY KEY,k INTEGER);CREATE TABLE c(cid INTEGER PRIMARY KEY,k INTEGER)")?;
    let queries = [
        "SELECT a.id,b.bid,c.cid FROM a LEFT JOIN (b INNER JOIN c ON b.k=c.k) ON a.k=b.k",
        "WITH RECURSIVE pairs(x) AS(SELECT k FROM a UNION SELECT k FROM b) SELECT x FROM pairs",
    ];
    for (i, q) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{q}')"
        ))?;
    }
    for sql in [
        "INSERT INTO a VALUES(1,1),(2,NULL)",
        "INSERT INTO b VALUES(1,1),(2,2)",
        "INSERT INTO c VALUES(1,1)",
        "UPDATE c SET k=2",
        "DELETE FROM b",
        "DELETE FROM a",
        "DELETE FROM c",
    ] {
        db.execute_batch(sql)?;
        for (i, q) in queries.iter().enumerate() {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM v{i}"))?,
                rows(&db, q)?,
                "{sql}: {q}"
            );
        }
    }
    Ok(())
}

#[test]
fn materialized_output_affinity_matches_ordinary_view_consumers() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,label TEXT)")?;
    let query="SELECT id,label,CAST(label AS INTEGER) AS numeric,label COLLATE NOCASE AS folded,(label) AS paren FROM a";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}');CREATE VIEW expected AS {query}"
    ))?;
    for sql in [
        "INSERT INTO a VALUES(1,'1'),(2,'2'),(3,'ALPHA'),(4,NULL)",
        "UPDATE a SET label='01' WHERE id=1",
        "UPDATE a SET label='alpha' WHERE id=3",
        "DELETE FROM a WHERE id=2",
    ] {
        db.execute_batch(sql)?;
        for predicate in [
            "label=1",
            "1=label",
            "label BETWEEN 1 AND 2",
            "label IN(1,2)",
            "id='1'",
            "id=label",
            "numeric='1'",
            "folded=1",
            "paren=1",
            "folded='alpha'",
        ] {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM result WHERE {predicate}"))?,
                rows(&db, &format!("SELECT * FROM expected WHERE {predicate}"))?,
                "{sql}: {predicate}"
            );
        }
    }
    Ok(())
}

fn recursion_database(schema: &str) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
    db.execute_batch(schema)?;
    Ok(db)
}
const GRAPH: &str = "CREATE TABLE roots(k INTEGER);CREATE TABLE edges(a INTEGER,b INTEGER);CREATE TABLE allowed(k INTEGER);CREATE TABLE blocked(k INTEGER)";
const GRAPH_MUTATIONS: [&str; 14] = [
    "INSERT INTO roots VALUES(1),(1),(9),(NULL)",
    "INSERT INTO edges VALUES(1,2),(2,3),(3,1),(3,4),(4,5),(5,4),(7,8),(NULL,6),(6,NULL)",
    "INSERT INTO allowed VALUES(1),(2),(3),(4),(5),(8);INSERT INTO blocked VALUES(3),(5)",
    "DELETE FROM edges WHERE a=2 AND b=3",
    "INSERT INTO edges VALUES(2,3)",
    "DELETE FROM roots WHERE k=1",
    "BEGIN;DELETE FROM edges WHERE a=3;INSERT INTO roots VALUES(7)",
    "ROLLBACK",
    "INSERT INTO roots VALUES(1);INSERT INTO edges VALUES(9,1)",
    "DELETE FROM edges WHERE a=3 AND b=1",
    "UPDATE edges SET b=1 WHERE a=5",
    "DELETE FROM allowed WHERE k=4;DELETE FROM blocked",
    "DELETE FROM edges",
    "DELETE FROM roots",
];
fn verify_recursion(query: &str) -> Result<()> {
    let db = recursion_database(GRAPH)?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
        query.replace('\'', "''")
    ))?;
    assert_eq!(
        rows(&db, "SELECT * FROM result")?,
        rows(&db, query)?,
        "empty"
    );
    for sql in GRAPH_MUTATIONS {
        db.execute_batch(sql)?;
        assert_eq!(
            rows(&db, "SELECT * FROM result")?,
            rows(&db, query)?,
            "{sql}"
        );
    }
    db.execute_batch("ALTER TABLE result RENAME TO renamed;INSERT INTO roots VALUES(4);INSERT INTO edges VALUES(4,5),(5,4)")?;
    assert_eq!(
        rows(&db, "SELECT * FROM renamed")?,
        rows(&db, query)?,
        "renamed"
    );
    db.execute_batch("DROP TABLE renamed")?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM sqlite_schema WHERE name LIKE '%result%' OR name LIKE '%renamed%'",[],|r| r.get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn row1_binary_closure_with_cycles() -> Result<()> {
    verify_recursion("WITH RECURSIVE path(src,dst) AS(SELECT a,b FROM edges UNION SELECT p.src,e.b FROM path p JOIN edges e ON p.dst=e.a) SELECT src,dst FROM path")
}

#[test]
fn row2_parity_over_roots() -> Result<()> {
    verify_recursion("WITH RECURSIVE parity(node,odd) AS(SELECT k,0 FROM roots UNION SELECT e.b,1-p.odd FROM parity p JOIN edges e ON p.node=e.a) SELECT node,odd FROM parity")
}

#[test]
fn row3_filtered_step_with_two_joins_and_distinct() -> Result<()> {
    verify_recursion("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT DISTINCT second.b FROM r JOIN edges first ON r.n=first.a JOIN edges second ON first.b=second.a JOIN allowed al ON al.k=second.b WHERE second.b<>r.n) SELECT n FROM r")?;
    verify_recursion("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM edges e,r WHERE r.n=e.a AND e.b IS NOT NULL) SELECT n FROM r")
}

#[test]
fn row4_min_distance_aggregate_after_bounded_recursion() -> Result<()> {
    verify_recursion("WITH RECURSIVE walk(node,dist) AS(SELECT k,0 FROM roots UNION SELECT e.b,w.dist+1 FROM walk w JOIN edges e ON w.node=e.a WHERE w.dist<3) SELECT node,MIN(dist) AS dist,COUNT(*) AS paths FROM walk GROUP BY node")
}

#[test]
fn row5_antijoin_and_exists_after_recursion() -> Result<()> {
    verify_recursion("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM r JOIN edges e ON r.n=e.a) SELECT n FROM r WHERE NOT EXISTS(SELECT 1 FROM blocked WHERE blocked.k=r.n)")?;
    verify_recursion("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM r JOIN edges e ON r.n=e.a) SELECT DISTINCT n FROM r WHERE EXISTS(SELECT 1 FROM allowed WHERE allowed.k=r.n)")
}

#[test]
fn row6_sequential_fixpoints_and_two_step_rules() -> Result<()> {
    verify_recursion("WITH RECURSIVE closed(src,dst) AS(SELECT a,b FROM edges UNION SELECT cl.src,e.b FROM closed cl JOIN edges e ON cl.dst=e.a),r(n) AS(SELECT k FROM roots UNION SELECT cl.dst FROM r JOIN closed cl ON r.n=cl.src) SELECT n FROM r")?;
    verify_recursion("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT k FROM allowed WHERE k>4 UNION SELECT e.b FROM r JOIN edges e ON r.n=e.a UNION SELECT e.a FROM r JOIN edges e ON r.n=e.b) SELECT n FROM r")
}

#[test]
fn rows7_and_8_named_rejections_leave_no_state() -> Result<()> {
    let db = recursion_database(GRAPH)?;
    for (query, message) in [
        ("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT max(e.b) FROM r JOIN edges e ON r.n=e.a) SELECT n FROM r", "recursive step may not aggregate or negate its own relation"),
        ("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM r JOIN edges e ON r.n=e.a WHERE NOT EXISTS(SELECT 1 FROM r r2 WHERE r2.n=e.b)) SELECT n FROM r", "recursive step may not aggregate or negate its own relation"),
        ("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM edges e WHERE e.a IN (SELECT n FROM r)) SELECT n FROM r", "recursive step may not aggregate or negate its own relation"),
        ("WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION ALL SELECT e.b FROM r JOIN edges e ON r.n=e.a WHERE e.b<5) SELECT n FROM r", "recursive UNION ALL unsupported"),
    ] {
        let error = db
            .execute_batch(&format!("CREATE VIRTUAL TABLE bad USING sqlite_ivm('{query}')"))
            .unwrap_err()
            .to_string();
        assert_eq!(error, message, "{query}");
        assert_eq!(
            db.query_row("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'bad%' OR name LIKE '__ivm_bad%'",[],|r| r.get::<_, i64>(0))?,
            0
        );
    }
    Ok(())
}

static STATEMENTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe extern "C" fn count_statement(
    event: std::ffi::c_uint,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if event == rusqlite::ffi::SQLITE_TRACE_STMT {
        STATEMENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    0
}
fn trace(db: &Connection, mask: std::ffi::c_uint) {
    unsafe {
        rusqlite::ffi::sqlite3_trace_v2(
            db.handle(),
            mask,
            if mask == 0 {
                None
            } else {
                Some(count_statement)
            },
            std::ptr::null_mut(),
        );
    }
}
fn closure_statements(chain: i64) -> Result<(usize, i64)> {
    let db = recursion_database("CREATE TABLE edges(a INTEGER,b INTEGER)")?;
    let query = "WITH RECURSIVE path(src,dst) AS(SELECT a,b FROM edges UNION SELECT p.src,e.b FROM path p JOIN edges e ON p.dst=e.a) SELECT src,dst FROM path";
    db.execute_batch(&format!("WITH RECURSIVE n(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{chain}) INSERT INTO edges SELECT i,i+1 FROM n WHERE i<{chain}"))?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE closure USING sqlite_ivm('{query}')"
    ))?;
    let before: i64 = db.query_row("SELECT count(*) FROM closure", [], |r| r.get(0))?;
    trace(&db, rusqlite::ffi::SQLITE_TRACE_STMT);
    STATEMENTS.store(0, std::sync::atomic::Ordering::Relaxed);
    db.execute(
        "INSERT INTO edges VALUES(?1,?2)",
        rusqlite::params![chain, chain + 1],
    )?;
    let statements = STATEMENTS.load(std::sync::atomic::Ordering::Relaxed);
    trace(&db, 0);
    let after: i64 = db.query_row("SELECT count(*) FROM closure", [], |r| r.get(0))?;
    assert_eq!(
        after - before,
        chain,
        "new closure rows for one appended edge"
    );
    assert_eq!(rows(&db, "SELECT * FROM closure")?, rows(&db, query)?);
    Ok((statements, chain))
}

// COUNT test for the semi-naive law: one appended edge on an N-node chain derives
// N new closure rows; statements grow with those rows, never with the closure size.
#[test]
fn recursion_statement_count_is_linear_in_new_closure_rows() -> Result<()> {
    let (small, small_rows) = closure_statements(250)?;
    let (large, large_rows) = closure_statements(1000)?;
    let ratio = large as f64 / small as f64;
    let expected = large_rows as f64 / small_rows as f64;
    assert!(
        ratio < expected * 1.5,
        "statements {small} -> {large} (x{ratio:.2}) for rows {small_rows} -> {large_rows} (x{expected:.2}); quadratic would be x{:.1}",
        expected * expected
    );
    assert!(
        large < 8 * large_rows as usize + 64,
        "{large} statements for {large_rows} new rows"
    );
    Ok(())
}

#[test]
fn comma_join_star_predicate_matches_plain_query() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
    db.execute_batch(
        "CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER);
        CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER);
        CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER);
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO a SELECT x,x FROM s;
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO b SELECT x,x FROM s;
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO c SELECT x,x FROM s;",
    )?;
    let query = "SELECT a.id AS x,b.id AS y,c.id AS z FROM a,b,c WHERE b.k=a.k AND c.k=a.k";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"
    ))?;
    let compare = |tag: &str| -> Result<()> {
        assert_eq!(
            rows(&db, "SELECT * FROM result")?,
            rows(&db, query)?,
            "{tag}"
        );
        Ok(())
    };
    compare("empty")?;
    for sql in [
        "INSERT INTO a VALUES(1001,1001)",
        "INSERT INTO b VALUES(1001,1001)",
        "INSERT INTO c VALUES(1001,1001)",
        "UPDATE b SET k=2000 WHERE id=1",
        "DELETE FROM b WHERE k=2000",
        "DELETE FROM a WHERE id=2",
        "DELETE FROM c",
    ] {
        db.execute_batch(sql)?;
        compare(sql)?;
    }
    Ok(())
}

#[test]
fn comma_join_arrangements_hold_side_row_counts_never_the_product() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
    db.execute_batch(
        "CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER);
        CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER);
        CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER);
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO a SELECT x,x FROM s;
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO b SELECT x,x FROM s;
        WITH RECURSIVE s(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM s WHERE x<1000)
        INSERT INTO c SELECT x,x FROM s;",
    )?;
    let query = "SELECT a.id AS x,b.id AS y,c.id AS z FROM a,b,c WHERE b.k=a.k AND c.k=a.k";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"
    ))?;
    let arrangements = rows(
        &db,
        "SELECT object_name FROM main.__ivm_objects
        WHERE view_name='result' AND object_type='table' AND object_name LIKE 'result_op%'",
    )?
    .into_iter()
    .map(|r| match &r[0] {
        Value::Text(s) => s.clone(),
        v => panic!("unexpected object name {v:?}"),
    })
    .collect::<Vec<_>>();
    assert_eq!(arrangements.len(), 4, "{arrangements:?}");
    for name in arrangements {
        let (n, total): (i64, i64) = db.query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(__n),0) FROM main.\"{name}\""),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!((n, total), (1000, 1000), "{name}");
    }
    Ok(())
}
