use ivm_ir::{self, AnyValue, RelKind, Program, RelId, Row, Term, Ty, W};
use sqlite_ext::rusqlite::{self, Connection, OptionalExtension, functions::{Context, FunctionFlags}, types::Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Process-wide counts of dictionary work: calls of the dictionary UDFs, and term or key lookups
/// (UDF reads and install backfill). `DictCounts` records their growth on the settle span.
static UDF_CALLS: AtomicU64 = AtomicU64::new(0);
static TERM_LOOKUPS: AtomicU64 = AtomicU64::new(0);

/// Records the dictionary work done while it lives as `udf_calls`/`term_lookups` on `span`.
/// Counts are process-wide, so concurrent settles on other connections add to them.
pub(crate) struct DictCounts {
    span: tracing::Span,
    calls: u64,
    lookups: u64,
}

impl DictCounts {
    pub(crate) fn start(span: &tracing::Span) -> Self {
        Self { span: span.clone(), calls: UDF_CALLS.load(Relaxed), lookups: TERM_LOOKUPS.load(Relaxed) }
    }
}

impl Drop for DictCounts {
    fn drop(&mut self) {
        self.span.record("udf_calls", UDF_CALLS.load(Relaxed) - self.calls);
        self.span.record("term_lookups", TERM_LOOKUPS.load(Relaxed) - self.lookups);
    }
}

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
        CREATE TABLE IF NOT EXISTS ivm_text(id INTEGER PRIMARY KEY, text TEXT NOT NULL UNIQUE);\
        CREATE TABLE IF NOT EXISTS ivm_term_sortkey(id INTEGER PRIMARY KEY, key BLOB NOT NULL);")?;
    functor_id(db, STR_FUNCTOR)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_cell_dict(id INTEGER PRIMARY KEY, class INTEGER NOT NULL, payload INTEGER NOT NULL, UNIQUE(class,payload))")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ivm_blob_dict(id INTEGER PRIMARY KEY AUTOINCREMENT, bytes BLOB NOT NULL UNIQUE)")?;
    // Terms of an earlier install that predate stored keys; before any mint below moves the watermark.
    fill_keys(db)?;
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

/// Stores the sort key of every term without one, in id order. A term's arguments have smaller ids
/// than the term (mint allocates above the current maximum), so each child key is already known.
/// Install runs this once; afterwards every mint path stores its keys as it mints.
fn fill_keys(db: &Connection) -> rusqlite::Result<()> {
    let ids: Vec<i64> = db.prepare("SELECT id FROM ivm_term WHERE id>(SELECT coalesce(max(id),0) FROM ivm_term_sortkey) ORDER BY id")?
        .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let (mut keys, mut ctors) = (HashMap::new(), Ctors::new());
    for id in ids {
        let Some(term) = lookup(db, &mut ctors, id)? else { continue };
        let mut failure = None;
        let key = ivm_ir::term_key(&term, &mut |child| match keys.get(&child) {
            Some(key) => Vec::clone(key),
            None => stored_key(db, child).unwrap_or_else(|error| { failure = Some(error); vec![] }),
        });
        if let Some(error) = failure { return Err(error); }
        db.prepare_cached("INSERT INTO ivm_term_sortkey(id,key) VALUES (?1,?2)")?.execute((id, &key))?;
        keys.insert(id, key);
    }
    Ok(())
}

/// Stored sort key of `id`; ids that are not dictionary terms are atoms.
fn stored_key(db: &Connection, id: i64) -> rusqlite::Result<Vec<u8>> {
    TERM_LOOKUPS.fetch_add(1, Relaxed);
    let key: Option<Vec<u8>> = db.prepare_cached("SELECT key FROM ivm_term_sortkey WHERE id=?1")?
        .query_row([id], |r| r.get(0)).optional()?;
    Ok(key.unwrap_or_else(|| ivm_ir::atom_key(id)))
}

/// SQL value of the sort key of the Id cell `value`: one primary-key read of the key stored at mint.
/// `value` names an outer column (`cN`, `alias.cN`); a bare `id` or `key` would bind to the key table.
pub(crate) fn sort_key_sql(value: &str) -> String {
    format!("coalesce((SELECT ivm_sk.key FROM ivm_term_sortkey ivm_sk WHERE ivm_sk.id={value}),ivm_atom_key({value}))")
}

/// Statement that stores the sort keys of the constructor terms `name` minted since the last fill.
/// Children are older terms, so their keys are stored; `ivm_ctor_sortkey` assembles the key in Rust.
pub(crate) fn ctor_keys_sql(name: &str, types: &[Ty]) -> String {
    let codes: String = types.iter().map(|ty| ty_code(*ty).to_string()).collect();
    let args: String = types.iter().enumerate().map(|(i, ty)| match ty {
        Ty::Id | Ty::Text => format!(",{}", sort_key_sql(&format!("k.c{}", i + 1))),
        _ => format!(",k.c{}", i + 1),
    }).collect();
    format!(
        "INSERT INTO ivm_term_sortkey(id,key) SELECT k.c0, ivm_ctor_sortkey('{}','{codes}'{args}) FROM {} k WHERE k.c0>(SELECT coalesce(max(id),0) FROM ivm_term_sortkey)",
        name.replace('\'', "''"), crate::catalog::quote(ctor_table(name)),
    )
}

/// Statement that stores the sort keys of the texts minted since the last fill.
const TEXT_KEYS_SQL: &str = "INSERT INTO ivm_term_sortkey(id,key) SELECT id, ivm_text_sortkey(text) FROM ivm_text WHERE id>(SELECT coalesce(max(id),0) FROM ivm_term_sortkey)";

fn text_key(text: &str) -> Vec<u8> {
    ivm_ir::term_key(&Term { functor: String::new(), args: vec![], types: vec![], text: Some(text.to_owned()) }, &mut |_| vec![])
}

/// Aux-data code shared by the dictionary functions of one prepared statement. A negative code
/// keeps the pointer for every call of the statement's run (all rows, all call sites); SQLite frees
/// it when the statement resets or finalizes.
const DICT_AUX: std::os::raw::c_int = -0x4956;

/// Dictionary reader of one statement run: a non-owning view of the calling connection whose
/// statement cache (and constructor-table SQL per functor) lives until the calling statement
/// resets. Each lookup statement is prepared once per run, not once per call; the views are
/// finalized at reset, before the host can close the connection.
struct Dict {
    db: Connection,
    ctors: Ctors,
    calls: u64,
    lookups: u64,
}

impl Drop for Dict {
    /// One event per statement run that used the dictionary, under that statement's span.
    fn drop(&mut self) {
        tracing::debug!(target: crate::observe::TARGET, udf_calls = self.calls,
            term_lookups = TERM_LOOKUPS.load(Relaxed) - self.lookups, "dictionary_run");
    }
}

/// Functor id -> (row select of its constructor table, argument types).
type Ctors = HashMap<i64, (String, Vec<Ty>)>;

fn with_dict<T>(ctx: &Context<'_>, f: impl FnOnce(&mut Dict) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let dict = match ctx.get_aux::<Mutex<Dict>>(DICT_AUX)? {
        Some(dict) => dict,
        None => {
            // SAFETY: the view never closes the handle and is dropped with the statement's aux data.
            let db = unsafe { Connection::from_handle(ctx.get_connection()?.handle())? };
            ctx.set_aux(DICT_AUX, Mutex::new(Dict { db, ctors: Ctors::new(), calls: 0, lookups: TERM_LOOKUPS.load(Relaxed) }))?
        }
    };
    let mut dict: std::sync::MutexGuard<'_, Dict> = dict.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
    UDF_CALLS.fetch_add(1, Relaxed);
    dict.calls += 1;
    f(&mut dict)
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
        let id = ctx.get(0)?;
        with_dict(ctx, |dict| text(&dict.db, id)?.ok_or(rusqlite::Error::InvalidQuery))
    })?;
    db.create_scalar_function("ivm_any_value", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let id = ctx.get(0)?;
        with_dict(ctx, |dict| decode_any(&dict.db, id))
    })?;
    db.create_scalar_function("ivm_any_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let id = ctx.get(0)?;
        with_dict(ctx, |dict| any_key(&dict.db, id))
    })?;
    db.create_scalar_function("ivm_any_id", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let value = match ctx.get::<Value>(0)? {
            Value::Null => AnyValue::Null,
            Value::Integer(v) => AnyValue::Integer(v),
            Value::Real(v) => AnyValue::Real(v.to_bits()),
            Value::Text(v) => AnyValue::Text(v),
            Value::Blob(v) => AnyValue::Blob(v),
        };
        with_dict(ctx, |dict| intern_any_value(&dict.db, &value))
    })?;
    db.create_scalar_function("ivm_text_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let id = ctx.get(0)?;
        with_dict(ctx, |dict| text(&dict.db, id)?.ok_or(rusqlite::Error::InvalidQuery))
    })?;
    db.create_scalar_function("ivm_term_lt", 2, FunctionFlags::SQLITE_UTF8, |ctx| {
        let (a, b): (i64, i64) = (ctx.get(0)?, ctx.get(1)?);
        // SQLite permits nested read statements from a scalar function on this connection.
        with_dict(ctx, |dict| {
            let mut failure = None;
            let order = ivm_ir::compare(a, b, &mut |id| match lookup(&dict.db, &mut dict.ctors, id) {
                Ok(term) => term,
                Err(error) => { failure = Some(error); None }
            });
            if let Some(error) = failure { return Err(error); }
            Ok((order == Ordering::Less) as i64)
        })
    })?;
    db.create_scalar_function("ivm_term_key", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let id: i64 = ctx.get(0)?;
        with_dict(ctx, |dict| stored_key(&dict.db, id))
    })?;
    let pure = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("ivm_atom_key", 1, pure, |ctx| Ok(ivm_ir::atom_key(ctx.get(0)?)))?;
    db.create_scalar_function("ivm_text_sortkey", 1, pure, |ctx| Ok(text_key(&ctx.get::<String>(0)?)))?;
    // (functor, type codes, then per argument: its value, or the child's sort key for Id/Text).
    db.create_scalar_function("ivm_ctor_sortkey", -1, pure, |ctx| {
        let functor: String = ctx.get(0)?;
        let types = ctx.get::<String>(1)?.bytes().map(|code| ty_from_code((code - b'0') as i64)).collect::<rusqlite::Result<Vec<_>>>()?;
        if ctx.len() != 2 + types.len() { return Err(rusqlite::Error::InvalidQuery); }
        let mut args = Vec::with_capacity(types.len());
        let mut children = Vec::new();
        for (at, ty) in types.iter().enumerate() {
            match ty {
                Ty::Id | Ty::Text => { args.push(0); children.push(ctx.get::<Vec<u8>>(2 + at)?); }
                _ => args.push(ctx.get::<i64>(2 + at)?),
            }
        }
        let mut children = children.into_iter();
        let term = Term { functor, args, types, text: None };
        Ok(ivm_ir::term_key(&term, &mut |_| children.next().unwrap_or_default()))
    })?;
    db.create_scalar_function("ivm_text_id", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let value: String = ctx.get(0)?;
        with_dict(ctx, |dict| text_id(&dict.db, &value)?.ok_or(rusqlite::Error::InvalidQuery))
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
    db.prepare_cached("INSERT INTO ivm_cell_dict(class,payload) VALUES (?1,?2) ON CONFLICT DO NOTHING")?.execute((class, payload))?;
    db.prepare_cached("SELECT id FROM ivm_cell_dict WHERE class=?1 AND payload=?2")?.query_row((class, payload), |r| r.get(0))
}

fn intern_blob(db: &Connection, bytes: &[u8]) -> rusqlite::Result<i64> {
    db.prepare_cached("INSERT INTO ivm_blob_dict(bytes) VALUES (?1) ON CONFLICT DO NOTHING")?.execute([bytes])?;
    db.prepare_cached("SELECT id FROM ivm_blob_dict WHERE bytes=?1")?.query_row([bytes], |r| r.get(0))
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
        db.prepare_cached("SELECT id FROM ivm_cell_dict WHERE class=1 AND payload=?1")?.query_row([n as i64], |r| r.get(0))
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
    let (class, payload): (i64, i64) = db.prepare_cached("SELECT class,payload FROM ivm_cell_dict WHERE id=?1")?.query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    match class {
        0 => Ok(Value::Null),
        1 => Ok(Value::Integer(payload)),
        2 => Ok(Value::Real(f64::from_bits(payload as u64))),
        3 => Ok(Value::Text(text(db, payload)?.ok_or(rusqlite::Error::InvalidQuery)?)),
        4 => db.prepare_cached("SELECT bytes FROM ivm_blob_dict WHERE id=?1")?.query_row([payload], |r| r.get::<_, Vec<u8>>(0)).map(Value::Blob),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

pub(crate) fn text_id(db: &Connection, value: &str) -> rusqlite::Result<Option<i64>> {
    db.prepare_cached("SELECT id FROM ivm_text WHERE text=?1")?.query_row([value], |r| r.get(0)).optional()
}

pub(crate) fn text(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.prepare_cached("SELECT text FROM ivm_text WHERE id=?1")?.query_row([id], |r| r.get(0)).optional()
}

/// SQL statements that mint every `text` of `source` (a query yielding column `text`) missing from
/// `ivm_text`: ids are allocated above the current `ivm_term` maximum, then the new ids get their
/// `ivm_term` rows and stored sort keys. `source` is a CTE-free select.
pub(crate) fn mint_texts_sql(source: &str) -> [String; 3] {
    [
        format!("INSERT INTO ivm_text(id,text) SELECT (SELECT coalesce(max(id),0) FROM ivm_term)+row_number() OVER (ORDER BY text), text FROM (SELECT DISTINCT text FROM ({source}) WHERE text NOT IN (SELECT text FROM ivm_text))"),
        format!("{AFTER_MINT}INSERT INTO ivm_term(id,functor_id) SELECT id, (SELECT id FROM ivm_functor WHERE name='{STR_FUNCTOR}') FROM ivm_text WHERE id>(SELECT coalesce(max(id),0) FROM ivm_term)"),
        format!("{AFTER_MINT}{TEXT_KEYS_SQL}"),
    ]
}

/// Prefix of a statement that registers the rows the statement before it minted; the
/// settle runs it only when that statement inserted rows.
pub(crate) const AFTER_MINT: &str = "/*after mint*/ ";

/// Interns `name(args)` as a Mint of that row would: an existing row keeps its id, a new one
/// takes the next `ivm_term` id and gets its `ivm_term` and sort-key rows.
pub(crate) fn intern_term(db: &Connection, name: &str, types: &[Ty], args: &[i64]) -> rusqlite::Result<i64> {
    let table = crate::catalog::quote(ctor_table(name));
    let lookup = if args.is_empty() {
        format!("SELECT c0 FROM {table} LIMIT 1")
    } else {
        format!("SELECT c0 FROM {table} WHERE {}", (1..=args.len()).map(|i| format!("c{i}=?{i}")).collect::<Vec<_>>().join(" AND "))
    };
    if let Some(id) = db.prepare_cached(&lookup)?.query_row(rusqlite::params_from_iter(args), |r| r.get(0)).optional()? {
        return Ok(id);
    }
    let id: i64 = db.prepare_cached(
        "INSERT INTO ivm_term(id,functor_id) SELECT coalesce(max(id),0)+1, (SELECT id FROM ivm_functor WHERE name=?1) FROM ivm_term RETURNING id")?
        .query_row([name], |r| r.get(0))?;
    let insert = format!(
        "INSERT INTO {table}(c0{}) VALUES (?1{})",
        (1..=args.len()).map(|i| format!(",c{i}")).collect::<String>(),
        (2..=args.len() + 1).map(|i| format!(",?{i}")).collect::<String>(),
    );
    db.prepare_cached(&insert)?.execute(rusqlite::params_from_iter(std::iter::once(&id).chain(args)))?;
    db.prepare_cached(&ctor_keys_sql(name, types))?.execute([])?;
    Ok(id)
}

/// Interns `value` whole: one `ivm_term`, `ivm_text` and sort-key row. Decompose mints the head and
/// rest of a string in the frontier that reads it.
pub(crate) fn intern_text(db: &Connection, value: &str) -> rusqlite::Result<i64> {
    if let Some(id) = text_id(db, value)? { return Ok(id); }
    let id: i64 = db.prepare_cached(
        "INSERT INTO ivm_term(id,functor_id) SELECT coalesce(max(id),0)+1, (SELECT id FROM ivm_functor WHERE name=?1) FROM ivm_term RETURNING id")?
        .query_row([STR_FUNCTOR], |r| r.get(0))?;
    db.prepare_cached("INSERT INTO ivm_text(id,text) VALUES (?1,?2)")?.execute((id, value))?;
    db.prepare_cached("INSERT INTO ivm_term_sortkey(id,key) VALUES (?1,?2)")?.execute((id, text_key(value)))?;
    Ok(id)
}

/// One term by id. `ctors` memoizes each functor's row select and types, so the constructor-table
/// name is formatted once per functor per cache.
fn lookup(db: &Connection, ctors: &mut Ctors, id: i64) -> rusqlite::Result<Option<Term>> {
    TERM_LOOKUPS.fetch_add(1, Relaxed);
    let row: Option<(i64, String, Option<String>)> = db.prepare_cached(
        "SELECT f.id,f.name,x.text FROM ivm_term t JOIN ivm_functor f ON f.id=t.functor_id LEFT JOIN ivm_text x ON x.id=t.id WHERE t.id=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    let Some((fid, functor, text)) = row else { return Ok(None); };
    if text.is_some() {
        return Ok(Some(Term { functor: String::new(), args: vec![], types: vec![], text }));
    }
    if !ctors.contains_key(&fid) {
        let types: Vec<Ty> = db.prepare_cached("SELECT ty FROM ivm_functor_col WHERE functor_id=?1 ORDER BY pos")?
            .query_map([fid], |r| r.get::<_, i64>(0))?.map(|code| ty_from_code(code?)).collect::<rusqlite::Result<_>>()?;
        let columns = (1..=types.len()).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
        let select = format!("SELECT {columns} FROM {} WHERE c0=?1", crate::catalog::quote(ctor_table(&functor)));
        ctors.insert(fid, (select, types));
    }
    let (select, types) = &ctors[&fid];
    let args: Row = if types.is_empty() { vec![] } else {
        db.prepare_cached(select)?
            .query_row([id], |r| (0..types.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>())?
    };
    Ok(Some(Term { functor, args, types: types.clone(), text: None }))
}

pub(crate) fn snapshot(db: &Connection, ir: &Program, functor: RelId) -> rusqlite::Result<Vec<(Row, W)>> {
    let rel = ir.rel(functor).filter(|r| r.kind == RelKind::Constructor).ok_or(rusqlite::Error::InvalidQuery)?;
    let table = crate::catalog::quote(ctor_table(&rel.name));
    let mut stmt = db.prepare_cached(&format!("SELECT * FROM {table} ORDER BY c0"))?;
    let rows = stmt.query_map([], |r| Ok(((0..rel.cols.len()).map(|i| r.get(i)).collect::<rusqlite::Result<Row>>()?, 1)))?
        .collect();
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    static STATEMENTS: AtomicUsize = AtomicUsize::new(0);
    fn count(_: rusqlite::trace::TraceEvent<'_>) { STATEMENTS.fetch_add(1, Relaxed); }

    static PREPARED_TEXT_READS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn authorize(
        _: *mut std::ffi::c_void, action: std::os::raw::c_int, table: *const std::os::raw::c_char,
        column: *const std::os::raw::c_char, _: *const std::os::raw::c_char, _: *const std::os::raw::c_char,
    ) -> std::os::raw::c_int {
        let name = |p: *const std::os::raw::c_char| if p.is_null() { "" } else { unsafe { std::ffi::CStr::from_ptr(p) }.to_str().unwrap_or("") };
        if action == rusqlite::ffi::SQLITE_READ && name(table) == "ivm_text" && name(column) == "text" {
            PREPARED_TEXT_READS.fetch_add(1, Relaxed);
        }
        rusqlite::ffi::SQLITE_OK
    }

    /// Named budget: texts read per statement run.
    const TEXTS: usize = 50;

    /// A dictionary function prepares its lookup once per statement run, not once per row, and its
    /// cached lookups are finalized when the run ends, so the host closes the connection cleanly.
    #[test]
    fn dictionary_function_prepares_once_per_statement_run() {
        let db = Connection::open_in_memory().unwrap();
        let ir = Program { texts: (0..TEXTS).map(|i| format!("t{i}")).collect(), rels: vec![], nodes: vec![], strata: vec![], outputs: vec![] };
        install(&db, &ir).unwrap();
        let read = |db: &Connection| db.prepare("SELECT ivm_text_value(id) FROM ivm_text").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().count();
        let open = |db: &Connection| {
            let mut stmt = std::ptr::null_mut();
            let mut count = 0;
            loop {
                stmt = unsafe { rusqlite::ffi::sqlite3_next_stmt(db.handle(), stmt) };
                if stmt.is_null() { return count; }
                count += 1;
            }
        };
        let texts = db.query_row("SELECT count(*) FROM ivm_text", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
        let before = open(&db);
        unsafe { rusqlite::ffi::sqlite3_set_authorizer(db.handle(), Some(authorize), std::ptr::null_mut()) };
        PREPARED_TEXT_READS.store(0, Relaxed);
        let rows = (read(&db), read(&db));
        let prepares = PREPARED_TEXT_READS.load(Relaxed);
        unsafe { rusqlite::ffi::sqlite3_set_authorizer(db.handle(), None, std::ptr::null_mut()) };
        let after = open(&db);
        assert_eq!((rows, prepares, after - before, db.close().is_ok()), ((texts, texts), 2, 0, true));
    }

    /// Named budget: nesting depth of each chain.
    const DEPTH: i64 = 40;
    /// Named budget: chains minted.
    const CHAINS: i64 = 6;

    /// Keys stored at mint equal the recursive `sort_key`, and ordering every term reads them in one
    /// statement: no per-term statements, no recursion through the dictionary.
    #[test]
    fn stored_keys_equal_recursive_keys_and_order_in_one_statement() {
        let db = Connection::open_in_memory().unwrap();
        let wrap = vec![Ty::Id, Ty::Id, Ty::Text, Ty::Int];
        let ir = Program {
            texts: vec!["b".into(), "a".into()],
            rels: vec![ivm_ir::Relation { id: 0, name: "wrap".into(), cols: wrap.clone(), kind: RelKind::Constructor }],
            nodes: vec![], strata: vec![], outputs: vec![],
        };
        install(&db, &ir).unwrap();
        let ctor = crate::catalog::quote(ctor_table("wrap"));
        let fid: i64 = db.query_row("SELECT id FROM ivm_functor WHERE name='wrap'", [], |r| r.get(0)).unwrap();
        let keys_sql = ctor_keys_sql("wrap", &wrap[1..]);
        let label = |chain: i64| text_id(&db, if chain % 2 == 0 { "a" } else { "b" }).unwrap().unwrap();
        for chain in 0..CHAINS {
            let mut prev = -1 - chain;
            for level in 0..DEPTH {
                let id: i64 = db.query_row("SELECT coalesce(max(id),0)+1 FROM ivm_term", [], |r| r.get(0)).unwrap();
                db.execute(&format!("INSERT INTO {ctor}(c0,c1,c2,c3) VALUES (?1,?2,?3,?4)"), (id, prev, label(chain), level % 3)).unwrap();
                db.execute("INSERT INTO ivm_term(id,functor_id) VALUES (?1,?2)", (id, fid)).unwrap();
                db.execute(&keys_sql, []).unwrap();
                prev = id;
            }
        }
        let stored: Vec<(i64, Vec<u8>)> = db.prepare("SELECT id, key FROM ivm_term_sortkey ORDER BY id").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        let recursive: Vec<(i64, Vec<u8>)> = stored.iter()
            .map(|(id, _)| (*id, ivm_ir::sort_key(*id, &mut |child| lookup(&db, &mut Ctors::new(), child).unwrap())))
            .collect();
        let terms: i64 = db.query_row("SELECT count(*) FROM ivm_term", [], |r| r.get(0)).unwrap();
        db.trace_v2(rusqlite::trace::TraceEventCodes::SQLITE_TRACE_PROFILE, Some(count));
        STATEMENTS.store(0, Relaxed);
        let ordered: Vec<i64> = db.prepare(&format!("SELECT t.id FROM ivm_term t ORDER BY {}", sort_key_sql("t.id"))).unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        let statements = STATEMENTS.load(Relaxed);
        db.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
        let mut expected = recursive.clone();
        expected.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            (stored.len() as i64, stored == recursive, statements, ordered),
            (terms, true, 1, expected.into_iter().map(|(id, _)| id).collect::<Vec<_>>()),
        );
    }
}
