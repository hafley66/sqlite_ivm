use rusqlite::{Connection, Result};
pub fn register(db: &Connection) -> Result<()> {
    #[cfg(feature = "bench")]
    if let Ok(path) = std::env::var("IVM_NATIVE_EXTENSION") {
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(path, None::<&str>)?;
            db.load_extension_disable()?;
        }
        return Ok(());
    }
    sqlite_ivm::extension::register(db)
}
