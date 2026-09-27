use ivm_ir::{self, RelKind, Program, RelId, Row, Term, Ty, W};
use sqlite_ext::rusqlite::{self, Connection, OptionalExtension, functions::FunctionFlags};
use std::cmp::Ordering;

pub(crate) fn ctor_table(name: &str) -> String {
    let hex: String = name.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("ivm_ctor_{hex}")
}

pub(crate) fn install(db: &Connection, ir: &Program) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_term_dict(\
        id INTEGER PRIMARY KEY AUTOINCREMENT, functor TEXT NOT NULL, args TEXT NOT NULL, types TEXT NOT NULL,\
        UNIQUE(functor,args));\
        CREATE TABLE IF NOT EXISTS ivm_term_functors(name TEXT PRIMARY KEY, types TEXT NOT NULL);")?;
    for rel in ir.rels.iter().filter(|r| r.kind == RelKind::Constructor) {
        if rel.name.is_empty() || rel.cols.first() != Some(&Ty::Id) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let types = serde_json::to_string(&rel.cols[1..]).map_err(|_| rusqlite::Error::InvalidQuery)?;
        db.execute("INSERT INTO ivm_term_functors(name,types) VALUES (?1,?2) ON CONFLICT(name) DO NOTHING", (&rel.name, &types))?;
        let stored: String = db.query_row("SELECT types FROM ivm_term_functors WHERE name=?1", [&rel.name], |r| r.get(0))?;
        if stored != types { return Err(rusqlite::Error::InvalidQuery); }
        let table = crate::catalog::quote(ctor_table(&rel.name));
        let fields = (1..rel.cols.len()).map(|i| format!("c{i} INTEGER NOT NULL")).collect::<Vec<_>>();
        db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table}(c0 INTEGER PRIMARY KEY{});",
            if fields.is_empty() { String::new() } else { format!(",{}", fields.join(",")) }))?;
    }
    register(db)
}

pub(crate) fn register(db: &Connection) -> rusqlite::Result<()> {
    db.create_scalar_function("ivm_term_lt", 2, FunctionFlags::SQLITE_UTF8, |ctx| {
        let (a, b): (i64, i64) = (ctx.get(0)?, ctx.get(1)?);
        // SQLite permits nested read statements from a scalar function on this connection.
        let db = unsafe { ctx.get_connection()? };
        let mut failure = None;
        let order = ivm_ir::compare(a, b, &mut |id| match lookup(&db, id) {
            Ok(term) => term,
            Err(error) => { failure = Some(error); None }
        });
        if let Some(error) = failure { return Err(error); }
        Ok((order == Ordering::Less) as i64)
    })?;
    db.create_scalar_function("ivm_term_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let id: i64 = ctx.get(0)?;
        let db = unsafe { ctx.get_connection()? };
        let mut failure = None;
        let key = ivm_ir::sort_key(id, &mut |id| match lookup(&db, id) {
            Ok(term) => term,
            Err(error) => { failure = Some(error); None }
        });
        if let Some(error) = failure { return Err(error); }
        Ok(key)
    })
}

fn lookup(db: &Connection, id: i64) -> rusqlite::Result<Option<Term>> {
    let row: Option<(String, String, String)> = db.query_row(
        "SELECT functor,args,types FROM ivm_term_dict WHERE id=?1", [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    row.map(|(functor, args, types)| Ok(Term {
        functor,
        args: serde_json::from_str(&args).map_err(|_| rusqlite::Error::InvalidQuery)?,
        types: serde_json::from_str(&types).map_err(|_| rusqlite::Error::InvalidQuery)?,
    })).transpose()
}

pub(crate) fn snapshot(db: &Connection, ir: &Program, functor: RelId) -> rusqlite::Result<Vec<(Row, W)>> {
    let rel = ir.rel(functor).filter(|r| r.kind == RelKind::Constructor).ok_or(rusqlite::Error::InvalidQuery)?;
    let table = crate::catalog::quote(ctor_table(&rel.name));
    let mut stmt = db.prepare(&format!("SELECT * FROM {table} ORDER BY c0"))?;
    let rows = stmt.query_map([], |r| Ok(((0..rel.cols.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>()?, 1)))?
        .collect();
    rows
}
