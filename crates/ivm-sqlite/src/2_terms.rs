use ivm_ir::{self, AnyValue, RelKind, Program, RelId, Row, Term, Ty, W};
use sqlite_ext::rusqlite::{self, Connection, OptionalExtension, functions::FunctionFlags, types::Value};
use std::cmp::Ordering;

pub(crate) fn ctor_table(name: &str) -> String {
    let hex: String = name.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("ivm_ctor_{hex}")
}

/// Functor-dictionary name of string terms. String ids live in `ivm_term` and `ivm_text`.
pub(crate) const STR_FUNCTOR: &str = "@str";

/// `ivm_functor_col.ty` code of a column type.
fn ty_code(ty: Ty) -> i64 { ty as u8 as i64 }

fn ty_from_code(code: i64) -> rusqlite::Result<Ty> {
    [Ty::Int, Ty::Id, Ty::Text, Ty::Real, Ty::Any].into_iter().find(|ty| ty_code(*ty) == code).ok_or(rusqlite::Error::InvalidQuery)
}

fn functor_id(db: &Connection, name: &str) -> rusqlite::Result<i64> {
    db.execute("INSERT INTO ivm_functor(name) VALUES (?1) ON CONFLICT(name) DO NOTHING", [name])?;
    db.query_row("SELECT id FROM ivm_functor WHERE name=?1", [name], |r| r.get(0))
}

pub(crate) fn install(db: &Connection, ir: &Program) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_functor(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);\
        CREATE TABLE IF NOT EXISTS ivm_functor_col(functor_id INTEGER NOT NULL, pos INTEGER NOT NULL, ty INTEGER NOT NULL, PRIMARY KEY(functor_id,pos)) WITHOUT ROWID;\
        CREATE TABLE IF NOT EXISTS ivm_term(id INTEGER PRIMARY KEY, functor_id INTEGER NOT NULL);\
        CREATE TABLE IF NOT EXISTS ivm_text(id INTEGER PRIMARY KEY, text TEXT NOT NULL UNIQUE);")?;
    functor_id(db, STR_FUNCTOR)?;
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
        let fid = functor_id(db, &rel.name)?;
        let stored: Vec<(i64, i64)> = db.prepare("SELECT pos,ty FROM ivm_functor_col WHERE functor_id=?1 ORDER BY pos")?
            .query_map([fid], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let declared: Vec<(i64, i64)> = rel.cols[1..].iter().enumerate().map(|(i, ty)| (i as i64 + 1, ty_code(*ty))).collect();
        if stored.is_empty() {
            for (pos, ty) in &declared {
                db.execute("INSERT INTO ivm_functor_col(functor_id,pos,ty) VALUES (?1,?2,?3)", (fid, pos, ty))?;
            }
        } else if stored != declared { return Err(rusqlite::Error::InvalidQuery); }
        let table = crate::catalog::quote(ctor_table(&rel.name));
        let fields = (1..rel.cols.len()).map(|i| format!("c{i} INTEGER NOT NULL")).collect::<Vec<_>>();
        let unique = (1..rel.cols.len()).map(|i| format!("c{i}")).collect::<Vec<_>>();
        db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table}(c0 INTEGER PRIMARY KEY{}{});",
            if fields.is_empty() { String::new() } else { format!(",{}", fields.join(",")) },
            if unique.is_empty() { String::new() } else { format!(",UNIQUE({})", unique.join(",")) }))?;
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
            Some(ivm_ir::StrOut::Holds) => Value::Integer(1),
            None => Value::Null,
        })
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
    db.query_row("SELECT id FROM ivm_text WHERE text=?1", [value], |r| r.get(0)).optional()
}

pub(crate) fn text(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.query_row("SELECT text FROM ivm_text WHERE id=?1", [id], |r| r.get(0)).optional()
}

/// SQL pair that mints every `text` of `source` (a query yielding column `text`) missing from
/// `ivm_text`: ids are allocated above the current `ivm_term` maximum, then the new ids get their
/// `ivm_term` rows. `source` is a CTE-free select; `with` is an optional `WITH ...` prefix.
pub(crate) fn mint_texts_sql(with: &str, source: &str) -> [String; 2] {
    [
        format!("{with} INSERT INTO ivm_text(id,text) SELECT (SELECT coalesce(max(id),0) FROM ivm_term)+row_number() OVER (ORDER BY text), text FROM (SELECT DISTINCT text FROM ({source}) WHERE text NOT IN (SELECT text FROM ivm_text))"),
        format!("INSERT INTO ivm_term(id,functor_id) SELECT id, (SELECT id FROM ivm_functor WHERE name='{STR_FUNCTOR}') FROM ivm_text WHERE id>(SELECT coalesce(max(id),0) FROM ivm_term)"),
    ]
}

pub(crate) fn intern_text(db: &Connection, value: &str) -> rusqlite::Result<i64> {
    if let Some(id) = text_id(db, value)? { return Ok(id); }
    if let Some(first) = value.chars().next() {
        let (head, rest) = value.split_at(first.len_utf8());
        intern_text(db, rest)?;
        if !rest.is_empty() { intern_text(db, head)?; }
    }
    let id: i64 = db.query_row(
        "INSERT INTO ivm_term(id,functor_id) SELECT coalesce(max(id),0)+1, (SELECT id FROM ivm_functor WHERE name=?1) FROM ivm_term RETURNING id",
        [STR_FUNCTOR], |r| r.get(0))?;
    db.execute("INSERT INTO ivm_text(id,text) VALUES (?1,?2)", (id, value))?;
    Ok(id)
}

fn lookup(db: &Connection, id: i64) -> rusqlite::Result<Option<Term>> {
    let row: Option<(i64, String, Option<String>)> = db.prepare_cached(
        "SELECT f.id,f.name,x.text FROM ivm_term t JOIN ivm_functor f ON f.id=t.functor_id LEFT JOIN ivm_text x ON x.id=t.id WHERE t.id=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    let Some((fid, functor, text)) = row else { return Ok(None); };
    if text.is_some() {
        return Ok(Some(Term { functor: String::new(), args: vec![], types: vec![], text, split: None }));
    }
    let types: Vec<Ty> = db.prepare_cached("SELECT ty FROM ivm_functor_col WHERE functor_id=?1 ORDER BY pos")?
        .query_map([fid], |r| r.get::<_, i64>(0))?.map(|code| ty_from_code(code?)).collect::<rusqlite::Result<_>>()?;
    let args: Row = if types.is_empty() { vec![] } else {
        let table = crate::catalog::quote(ctor_table(&functor));
        let columns = (1..=types.len()).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
        db.prepare_cached(&format!("SELECT {columns} FROM {table} WHERE c0=?1"))?
            .query_row([id], |r| (0..types.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>())?
    };
    Ok(Some(Term { functor, args, types, text: None, split: None }))
}

pub(crate) fn snapshot(db: &Connection, ir: &Program, functor: RelId) -> rusqlite::Result<Vec<(Row, W)>> {
    let rel = ir.rel(functor).filter(|r| r.kind == RelKind::Constructor).ok_or(rusqlite::Error::InvalidQuery)?;
    let table = crate::catalog::quote(ctor_table(&rel.name));
    let mut stmt = db.prepare_cached(&format!("SELECT * FROM {table} ORDER BY c0"))?;
    let rows = stmt.query_map([], |r| Ok(((0..rel.cols.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>()?, 1)))?
        .collect();
    rows
}
