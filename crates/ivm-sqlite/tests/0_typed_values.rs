use ivm_sqlite::{Cell, Frontier, Program};
use sqlite_ext::rusqlite::Connection;

#[test]
fn sql_text_storage_classes_survive_settle_and_reopen() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE mixed(k);
         CREATE TABLE jobr(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost REAL NOT NULL);
         INSERT INTO mixed VALUES (5),('5');
         INSERT INTO jobr VALUES (1,10,2.5);",
    ).unwrap();
    let keys = Program::install(&db, "typed_keys", "SELECT k FROM mixed").unwrap();
    let sum = Program::install(&db, "typed_sum", "SELECT team, count(*) AS n, sum(cost) AS total FROM jobr GROUP BY team").unwrap();
    db.execute_batch("INSERT INTO mixed VALUES ('a'),(2.5); INSERT INTO jobr VALUES (2,10,1.0);").unwrap();
    assert_eq!(keys.snapshot(&db).unwrap().into_iter().map(|row| row.0).collect::<Vec<_>>(), vec![
        vec![Cell::Real(2.5)], vec![Cell::Integer(5)], vec![Cell::Text("5".into())], vec![Cell::Text("a".into())],
    ]);
    assert_eq!(sum.snapshot(&db).unwrap()[0].0, vec![Cell::Integer(10), Cell::Integer(2), Cell::Real(3.5)]);
    drop(keys);
    drop(sum);
    let keys = Program::open(&db, "typed_keys").unwrap();
    let sum = Program::open(&db, "typed_sum").unwrap();
    db.execute_batch("DELETE FROM mixed WHERE typeof(k)='text' AND k='5'; UPDATE jobr SET cost=4.0 WHERE id=2;").unwrap();
    assert_eq!(keys.snapshot(&db).unwrap().len(), 3);
    assert_eq!(sum.snapshot(&db).unwrap()[0].0, vec![Cell::Integer(10), Cell::Integer(2), Cell::Real(6.5)]);
    keys.teardown(&db).unwrap();
    sum.teardown(&db).unwrap();
}
