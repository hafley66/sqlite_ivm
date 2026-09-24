//! Statement issuance with SQL-byte accounting.
//!
//! Every engine statement goes through here: the SQLite-facing span work stays
//! in `sqlite_ext::statements` (one span per statement, `prepared` marked
//! fresh or cached), and this layer adds the two numbers the report records —
//! statements issued and SQL bytes issued.

use sqlite_ext::rusqlite::{self, Connection, Params, Row};

#[derive(Default)]
pub(crate) struct Meter {
    pub statements: u64,
    pub sql_bytes: u64,
}

impl Meter {
    fn metered(&mut self, sql: &str) {
        self.statements += 1;
        self.sql_bytes += sql.len() as u64;
    }

    /// One cached DML statement with parameters.
    pub fn exec<P: Params>(
        &mut self,
        conn: &Connection,
        phase: &str,
        object: &str,
        sql: &str,
        params: P,
    ) -> rusqlite::Result<usize> {
        self.metered(sql);
        sqlite_ext::statements::exec_cached(conn, phase, object, sql, params)
    }

    /// One fresh multi-statement batch (install/teardown DDL).
    pub fn batch(
        &mut self,
        conn: &Connection,
        phase: &str,
        object: &str,
        sql: &str,
    ) -> rusqlite::Result<()> {
        self.metered(sql);
        sqlite_ext::statements::batch(conn, phase, object, sql)
    }

    /// One cached query collecting every row.
    pub fn rows<T, P: Params>(
        &mut self,
        conn: &Connection,
        phase: &str,
        object: &str,
        sql: &str,
        params: P,
        f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        self.metered(sql);
        sqlite_ext::statements::query_map(conn, phase, object, sql, params, f)
    }

    /// One cached query expected to return at most one row.
    pub fn one<T, P: Params>(
        &mut self,
        conn: &Connection,
        phase: &str,
        object: &str,
        sql: &str,
        params: P,
        f: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Option<T>> {
        self.metered(sql);
        match sqlite_ext::statements::query_cached(conn, phase, object, sql, params, |row| {
            Ok(f(row).map(Some))
        }) {
            Ok(value) => Ok(value?),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
