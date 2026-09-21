use rusqlite::{Connection, Result};
pub fn register(db: &Connection) -> Result<()> {
    sqlite_ivm::extension::register(db)
}
