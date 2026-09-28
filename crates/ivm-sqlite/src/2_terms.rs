use ivm_ir::{self, AnyValue, RelKind, Program, RelId, Row, Term, Ty, W};
use sqlite_ext::rusqlite::{self, Connection, OptionalExtension, functions::FunctionFlags, types::Value};
use std::cmp::Ordering;

pub(crate) fn ctor_table(name: &str) -> String {
    let hex: String = name.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("ivm_ctor_{hex}")
}

pub(crate) fn install(db: &Connection, ir: &Program) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_term_dict(\
        id INTEGER PRIMARY KEY AUTOINCREMENT, functor TEXT NOT NULL, args TEXT NOT NULL, types TEXT NOT NULL,\
        text TEXT, head_id INTEGER, rest_id INTEGER,\
        UNIQUE(functor,args));\
        CREATE TABLE IF NOT EXISTS ivm_term_functors(name TEXT PRIMARY KEY, types TEXT NOT NULL);")?;
    let columns: Vec<String> = db.prepare("PRAGMA table_info(ivm_term_dict)")?
        .query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    for column in ["text", "head_id", "rest_id"] {
        if !columns.iter().any(|existing| existing == column) {
            db.execute_batch(&format!("ALTER TABLE ivm_term_dict ADD COLUMN {column} {}", if column == "text" { "TEXT" } else { "INTEGER" }))?;
        }
    }
    db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS ivm_term_text_unique ON ivm_term_dict(text)")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_cell_dict(id INTEGER PRIMARY KEY, class INTEGER NOT NULL, payload INTEGER NOT NULL, UNIQUE(class,payload))")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_blob_dict(id INTEGER PRIMARY KEY AUTOINCREMENT, bytes BLOB NOT NULL UNIQUE)")?;
    if ir.uses_strings() {
        intern_text(db, "")?;
        for text in &ir.texts { intern_text(db, text)?; }
    }
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
    if db.prepare("SELECT ivm_any_value(0)").is_ok() { return Ok(()); }
    db.create_scalar_function("ivm_real_value", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        Ok(f64::from_bits(ctx.get::<i64>(0)? as u64))
    })?;
    db.create_scalar_function("ivm_real_bits", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        Ok(ctx.get::<f64>(0)?.to_bits() as i64)
    })?;
    db.create_scalar_function("ivm_text_value", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        text(&db, ctx.get(0)?)?.ok_or(rusqlite::Error::InvalidQuery)
    })?;
    db.create_scalar_function("ivm_any_value", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        decode_any(&db, ctx.get(0)?)
    })?;
    db.create_scalar_function("ivm_any_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        any_key(&db, ctx.get(0)?)
    })?;
    db.create_scalar_function("ivm_any_id", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        let value = match ctx.get::<Value>(0)? {
            Value::Null => AnyValue::Null,
            Value::Integer(v) => AnyValue::Integer(v),
            Value::Real(v) => AnyValue::Real(v.to_bits()),
            Value::Text(v) => AnyValue::Text(v),
            Value::Blob(v) => AnyValue::Blob(v),
        };
        intern_any_value(&db, &value)
    })?;
    db.create_scalar_function("ivm_text_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        text(&db, ctx.get(0)?)?.ok_or(rusqlite::Error::InvalidQuery)
    })?;
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
    })?;
    db.create_scalar_function("ivm_text_id", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let value: String = ctx.get(0)?;
        let db = unsafe { ctx.get_connection()? };
        text_id(&db, &value)?.ok_or(rusqlite::Error::InvalidQuery)
    })?;
    db.create_scalar_function("ivm_str_op", -1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        use sqlite_ext::rusqlite::types::ValueRef;
        let name: String = ctx.get(0)?;
        let op = ivm_ir::StrOp::from_name(&name).ok_or(rusqlite::Error::InvalidQuery)?;
        let mut values = Vec::with_capacity(ctx.len() - 1);
        for at in 1..ctx.len() {
            values.push(match ctx.get_raw(at) {
                ValueRef::Text(bytes) => ivm_ir::StrVal::Text(std::str::from_utf8(bytes).map_err(|error| rusqlite::Error::Utf8Error(0, error))?),
                ValueRef::Integer(value) => ivm_ir::StrVal::Int(value),
                _ => return Ok(Value::Null),
            });
        }
        Ok(match op.apply(&values) {
            Some(ivm_ir::StrOut::Text(text)) => Value::Text(text),
            Some(ivm_ir::StrOut::Int(value)) => Value::Integer(value),
            None => Value::Null,
        })
    })?;
    db.create_scalar_function("ivm_str_head", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        db.query_row("SELECT head_id FROM ivm_term_dict WHERE id=?1 AND text<>''", [ctx.get::<i64>(0)?], |r| r.get::<_, i64>(0)).optional()
    })?;
    db.create_scalar_function("ivm_str_rest", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let db = unsafe { ctx.get_connection()? };
        db.query_row("SELECT rest_id FROM ivm_term_dict WHERE id=?1 AND text<>''", [ctx.get::<i64>(0)?], |r| r.get::<_, i64>(0)).optional()
    })?;
    db.create_scalar_function("ivm_str_head_text", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let value: String = ctx.get(0)?;
        Ok(value.chars().next().map(|c| c.to_string()))
    })?;
    db.create_scalar_function("ivm_str_rest_text", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let value: String = ctx.get(0)?;
        Ok(value.chars().next().map(|c| value[c.len_utf8()..].to_owned()))
    })
}

pub(crate) fn encode_value(db: &Connection, ty: Ty, value: &Value) -> rusqlite::Result<i64> {
    match (ty, value) {
        (Ty::Int | Ty::Id, Value::Integer(v)) => Ok(*v),
        (Ty::Real, Value::Real(v)) => Ok(v.to_bits() as i64),
        (Ty::Real, Value::Integer(v)) => Ok((*v as f64).to_bits() as i64),
        (Ty::Text, Value::Text(v)) => intern_text(db, v),
        (Ty::Any, Value::Integer(v)) => intern_any(db, 1, *v),
        (Ty::Any, Value::Real(v)) => intern_any_value(db, &AnyValue::Real(v.to_bits())),
        (Ty::Any, Value::Text(v)) => intern_any(db, 3, intern_text(db, v)?),
        (Ty::Any, Value::Null) => intern_any(db, 0, 0),
        (Ty::Any, Value::Blob(v)) => intern_any(db, 4, intern_blob(db, v)?),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn intern_any(db: &Connection, class: i64, payload: i64) -> rusqlite::Result<i64> {
    db.execute("INSERT INTO ivm_cell_dict(class,payload) VALUES (?1,?2) ON CONFLICT DO NOTHING", (class, payload))?;
    db.query_row("SELECT id FROM ivm_cell_dict WHERE class=?1 AND payload=?2", (class, payload), |r| r.get(0))
}

fn intern_blob(db: &Connection, bytes: &[u8]) -> rusqlite::Result<i64> {
    db.execute("INSERT INTO ivm_blob_dict(bytes) VALUES (?1) ON CONFLICT DO NOTHING", [bytes])?;
    db.query_row("SELECT id FROM ivm_blob_dict WHERE bytes=?1", [bytes], |r| r.get(0))
}

pub(crate) fn intern_any_value(db: &Connection, value: &AnyValue) -> rusqlite::Result<i64> {
    match value {
        AnyValue::Null => intern_any(db, 0, 0),
        AnyValue::Integer(v) => intern_any(db, 1, *v),
        AnyValue::Real(bits) => {
            let n = f64::from_bits(*bits);
            if n.is_nan() { return intern_any(db, 0, 0); }
            if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 && (n as i64) as f64 == n {
                intern_any(db, 1, n as i64)?;
            }
            intern_any(db, 2, *bits as i64)
        }
        AnyValue::Text(v) => intern_any(db, 3, intern_text(db, v)?),
        AnyValue::Blob(v) => intern_any(db, 4, intern_blob(db, v)?),
    }
}

fn any_key(db: &Connection, id: i64) -> rusqlite::Result<i64> {
    let AnyValue::Real(bits) = any_value(db, id)? else { return Ok(id); };
    let n = f64::from_bits(bits);
    if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 && (n as i64) as f64 == n {
        db.query_row("SELECT id FROM ivm_cell_dict WHERE class=1 AND payload=?1", [n as i64], |r| r.get(0))
    } else { Ok(id) }
}

pub(crate) fn any_value(db: &Connection, id: i64) -> rusqlite::Result<AnyValue> {
    Ok(match decode_any(db, id)? {
        Value::Null => AnyValue::Null,
        Value::Integer(v) => AnyValue::Integer(v),
        Value::Real(v) => AnyValue::Real(v.to_bits()),
        Value::Text(v) => AnyValue::Text(v),
        Value::Blob(v) => AnyValue::Blob(v),
    })
}

fn decode_any(db: &Connection, id: i64) -> rusqlite::Result<Value> {
    let (class, payload): (i64, i64) = db.query_row("SELECT class,payload FROM ivm_cell_dict WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    match class {
        0 => Ok(Value::Null),
        1 => Ok(Value::Integer(payload)),
        2 => Ok(Value::Real(f64::from_bits(payload as u64))),
        3 => Ok(Value::Text(text(db, payload)?.ok_or(rusqlite::Error::InvalidQuery)?)),
        4 => db.query_row("SELECT bytes FROM ivm_blob_dict WHERE id=?1", [payload], |r| r.get::<_, Vec<u8>>(0)).map(Value::Blob),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

pub(crate) fn text_id(db: &Connection, value: &str) -> rusqlite::Result<Option<i64>> {
    db.query_row("SELECT id FROM ivm_term_dict WHERE text=?1", [value], |r| r.get(0)).optional()
}

pub(crate) fn text(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.query_row("SELECT text FROM ivm_term_dict WHERE id=?1 AND text IS NOT NULL", [id], |r| r.get(0)).optional()
}

pub(crate) fn intern_text(db: &Connection, value: &str) -> rusqlite::Result<i64> {
    if let Some(id) = text_id(db, value)? { return Ok(id); }
    let split = if let Some(first) = value.chars().next() {
        let (head, rest) = value.split_at(first.len_utf8());
        let rest_id = intern_text(db, rest)?;
        Some((if rest.is_empty() { None } else { Some(intern_text(db, head)?) }, rest_id))
    } else { None };
    db.execute("INSERT INTO ivm_term_dict(functor,args,types,text) VALUES ('@str',json_array(?1),'[]',?1) ON CONFLICT DO NOTHING", [value])?;
    let id = text_id(db, value)?.ok_or(rusqlite::Error::InvalidQuery)?;
    if let Some((head, rest)) = split {
        db.execute("UPDATE ivm_term_dict SET head_id=?1,rest_id=?2 WHERE id=?3", (head.unwrap_or(id), rest, id))?;
    }
    Ok(id)
}

fn lookup(db: &Connection, id: i64) -> rusqlite::Result<Option<Term>> {
    let row: Option<(String, String, String, Option<String>, Option<i64>, Option<i64>)> = db.query_row(
        "SELECT functor,args,types,text,head_id,rest_id FROM ivm_term_dict WHERE id=?1", [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).optional()?;
    row.map(|(functor, args, types, text, head, rest)| Ok(Term {
        functor,
        args: if text.is_some() { vec![] } else { serde_json::from_str(&args).map_err(|_| rusqlite::Error::InvalidQuery)? },
        types: if text.is_some() { vec![] } else { serde_json::from_str(&types).map_err(|_| rusqlite::Error::InvalidQuery)? },
        text,
        split: head.zip(rest),
    })).transpose()
}

pub(crate) fn snapshot(db: &Connection, ir: &Program, functor: RelId) -> rusqlite::Result<Vec<(Row, W)>> {
    let rel = ir.rel(functor).filter(|r| r.kind == RelKind::Constructor).ok_or(rusqlite::Error::InvalidQuery)?;
    let table = crate::catalog::quote(ctor_table(&rel.name));
    let mut stmt = db.prepare_cached(&format!("SELECT * FROM {table} ORDER BY c0"))?;
    let rows = stmt.query_map([], |r| Ok(((0..rel.cols.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>()?, 1)))?
        .collect();
    rows
}
