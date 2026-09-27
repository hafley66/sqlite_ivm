use rusqlite::{types::Value, Connection};

pub fn sqlite_rows(db: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut stmt = db.prepare(sql).unwrap();
    let n = stmt.column_count();
    let mut rows = stmt.query_map([], |r| {
        (0..n).map(|i| r.get(i)).collect::<rusqlite::Result<Vec<Value>>>()
    }).unwrap().map(Result::unwrap).collect::<Vec<_>>();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}
