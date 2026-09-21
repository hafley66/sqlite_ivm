use crate::pending::{
    Pending, Policy, Staged, PENDING_BYTE_CAP, PENDING_CAP_DIAGNOSTIC, PENDING_MARK_DIAGNOSTIC,
};
use rusqlite::{ffi, types::ValueRef, vtab::*, Connection, Result};
use std::{
    borrow::Cow,
    ffi::{c_int, CStr, CString},
};

pub const MODULE_NAME: &CStr = c"lab_ivm";

/// Visible columns of the declaration: the group key and the two aggregates.
const VISIBLE: usize = 3;

const DECLARATION: &str = "CREATE TABLE x(g INTEGER,n INTEGER,s INTEGER,\
    __ivm_adding INTEGER HIDDEN,__ivm_g INTEGER HIDDEN,__ivm_v INTEGER HIDDEN)";

pub fn error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(message.into())))
}

pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// rusqlite 0.40.2 wires xBegin/xSync/xCommit/xRollback and leaves the three
// savepoint callbacks null, so the ABI descriptor is patched as at src/2_vtab.rs:15.
pub fn register(db: &Connection) -> Result<()> {
    const MODULE: Module<Table> = unsafe {
        let mut raw: ffi::sqlite3_module =
            std::mem::transmute(Module::<Table>::update_module_with_tx());
        raw.iVersion = 2;
        raw.xSavepoint = Some(savepoint);
        raw.xRelease = Some(release);
        raw.xRollbackTo = Some(rollback_to);
        std::mem::transmute(raw)
    };
    db.create_module(MODULE_NAME, &MODULE, None::<()>)
}

#[repr(C)]
pub struct Table {
    base: ffi::sqlite3_vtab,
    db: Connection, // Non-owning handle, valid for this virtual-table connection.
    name: String,
    pending: Pending,
    policy: Policy,
}

// FTS5 makes its cap tunable per table with the hashsize option (fts5.c:5465).
// Both knobs are table arguments here for the same reason: an untested bound.
fn arguments(args: &[&[u8]]) -> Result<(usize, Policy)> {
    let (mut cap, mut policy) = (PENDING_BYTE_CAP, Policy::Flush);
    for raw in args {
        let text = std::str::from_utf8(raw).map_err(|e| error(e.to_string()))?;
        let (key, value) = text
            .trim()
            .split_once('=')
            .ok_or_else(|| error("lab_ivm arguments are cap=<bytes> and policy=flush|mark"))?;
        match (key.trim(), value.trim()) {
            ("cap", bytes) => {
                cap = bytes
                    .parse::<usize>()
                    .map_err(|_| error("cap must be a byte count"))?
            }
            ("policy", "flush") => policy = Policy::Flush,
            ("policy", "mark") => policy = Policy::Mark,
            _ => {
                return Err(error(
                    "lab_ivm arguments are cap=<bytes> and policy=flush|mark",
                ))
            }
        }
    }
    Ok((cap, policy))
}

impl Table {
    fn attach(
        db: &mut VTabConnection,
        schema: &[u8],
        name: &[u8],
        args: &[&[u8]],
        create: bool,
    ) -> Result<(Cow<'static, CStr>, Self)> {
        if schema != b"main" {
            return Err(error("lab_ivm tables must be in main"));
        }
        let name = std::str::from_utf8(name)
            .map_err(|e| error(e.to_string()))?
            .to_string();
        let conn = unsafe { Connection::from_handle(db.handle())? };
        let (cap, policy) = arguments(args)?;
        if create {
            conn.execute_batch(&format!(
                "CREATE TABLE main.{}(g INTEGER PRIMARY KEY,n INTEGER NOT NULL,s INTEGER NOT NULL);
                 CREATE TABLE main.{}(sign INTEGER NOT NULL,g INTEGER NOT NULL,v INTEGER NOT NULL);",
                quote(&format!("{name}_state")),
                quote(&format!("{name}_pending"))
            ))?;
        }
        Ok((
            Cow::Borrowed(unsafe { CStr::from_bytes_with_nul_unchecked(b"") }),
            Self {
                base: ffi::sqlite3_vtab::default(),
                db: conn,
                name,
                pending: Pending::with_cap(cap),
                policy,
            },
        ))
    }

    fn state(&self) -> String {
        quote(&format!("{}_state", self.name))
    }

    fn staging(&self) -> String {
        quote(&format!("{}_pending", self.name))
    }

    fn flush(&mut self) -> Result<()> {
        let rows = self.pending.take();
        self.apply(rows)
    }

    fn spill(&mut self) -> Result<()> {
        let rows = self.pending.spill();
        self.apply(rows)
    }

    /// Drains a batch into the arrangement. One maintenance statement per call,
    /// whatever the row count; per-row SQL is the cheap staging insert.
    fn apply(&mut self, rows: Vec<Staged>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let _span = tracing::debug_span!("flush", rows = rows.len()).entered();
        let (state, staging) = (self.state(), self.staging());
        {
            let mut insert = self.db.prepare_cached(&format!(
                "INSERT INTO main.{staging}(sign,g,v) VALUES(?1,?2,?3)"
            ))?;
            for row in &rows {
                let _span = tracing::debug_span!("stage/insert").entered();
                insert.execute(rusqlite::params![row.sign, row.group, row.value])?;
            }
        }
        {
            let _span = tracing::debug_span!("maintain/upsert").entered();
            self.db
                .prepare_cached(&format!(
                    "INSERT INTO main.{state}(g,n,s)
                     SELECT * FROM (SELECT g,SUM(sign) AS n,SUM(sign*v) AS s
                     FROM main.{staging} GROUP BY g) WHERE 1
                     ON CONFLICT(g) DO UPDATE SET n={state}.n+excluded.n,s={state}.s+excluded.s"
                ))?
                .execute([])?;
        }
        {
            let _span = tracing::debug_span!("maintain/prune").entered();
            self.db
                .prepare_cached(&format!("DELETE FROM main.{state} WHERE n=0"))?
                .execute([])?;
        }
        self.db
            .prepare_cached(&format!("DELETE FROM main.{staging}"))?
            .execute([])?;
        Ok(())
    }
}

unsafe impl<'vtab> VTab<'vtab> for Table {
    type Aux = ();
    type Cursor = Cursor;
    fn connect(
        db: &mut VTabConnection,
        _: Option<&()>,
        _: &[u8],
        schema: &[u8],
        name: &[u8],
        args: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let (_, table) = Self::attach(db, schema, name, args, false)?;
        Ok((
            Cow::Owned(CString::new(DECLARATION).map_err(|e| error(e.to_string()))?),
            table,
        ))
    }
    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        info.set_estimated_cost(1_000_000.0);
        Ok(true)
    }
    fn open(&'vtab mut self) -> Result<Cursor> {
        Ok(Cursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            db: unsafe { self.db.handle() },
            state: self.state(),
            rows: Vec::new(),
            at: 0,
        })
    }
}

impl<'vtab> CreateVTab<'vtab> for Table {
    const KIND: VTabKind = VTabKind::Default;
    fn create(
        db: &mut VTabConnection,
        _: Option<&()>,
        _: &[u8],
        schema: &[u8],
        name: &[u8],
        args: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let (_, table) = Self::attach(db, schema, name, args, true)?;
        Ok((
            Cow::Owned(CString::new(DECLARATION).map_err(|e| error(e.to_string()))?),
            table,
        ))
    }
    fn destroy(&self) -> Result<()> {
        self.db.execute_batch(&format!(
            "DROP TABLE main.{};DROP TABLE main.{};",
            self.state(),
            self.staging()
        ))
    }
}

impl<'vtab> UpdateVTab<'vtab> for Table {
    fn delete(&mut self, _: ValueRef<'_>) -> Result<()> {
        Err(error("managed results are read-only"))
    }
    fn update(&mut self, _: &Updates<'_>) -> Result<()> {
        Err(error("managed results are read-only"))
    }
    fn insert(&mut self, args: &Inserts<'_>) -> Result<i64> {
        let _span = tracing::debug_span!("stage/append").entered();
        if args.len() != VISIBLE + 5 || args.iter().take(VISIBLE + 2).any(|v| v != ValueRef::Null) {
            return Err(error("managed results are read-only"));
        }
        let adding: i64 = args.get(VISIBLE + 2)?;
        if !(0..=1).contains(&adding) {
            return Err(error("invalid maintenance command"));
        }
        let row = Staged {
            sign: if adding == 1 { 1 } else { -1 },
            group: args.get(VISIBLE + 3)?,
            value: args.get(VISIBLE + 4)?,
        };
        if self.pending.push(row) {
            let _span = tracing::debug_span!("flush/cap").entered();
            tracing::warn!(
                rows = self.pending.len(),
                cap = self.pending.cap(),
                "{PENDING_CAP_DIAGNOSTIC}"
            );
            self.spill()?;
        }
        Ok(0)
    }
}

impl<'vtab> TransactionVTab<'vtab> for Table {
    fn begin(&mut self) -> Result<()> {
        let _span = tracing::debug_span!("begin").entered();
        Ok(())
    }
    fn sync(&mut self) -> Result<()> {
        let _span = tracing::debug_span!("sync").entered();
        self.flush()
    }
    // SQLite discards xCommit's return code, so a failure here would be silent.
    // xSync already drained the buffer; the diagnostic span proves it.
    fn commit(&mut self) -> Result<()> {
        let _span = tracing::debug_span!("commit").entered();
        if !self.pending.is_empty() {
            let _span = tracing::debug_span!("commit/pending_not_empty").entered();
        }
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        let _span = tracing::debug_span!("rollback/discard").entered();
        self.pending.discard();
        Ok(())
    }
}

fn dispatch(
    raw: *mut ffi::sqlite3_vtab,
    body: impl FnOnce(&mut Table) -> Result<()>,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { &mut *raw.cast::<Table>() };
        body(table)
    }))
    .unwrap_or_else(|_| Err(error("panic in virtual-table savepoint callback")));
    match result {
        Ok(()) => ffi::SQLITE_OK,
        Err(e) => unsafe { rusqlite::to_sqlite_error(&e, &mut (*raw).zErrMsg) },
    }
}

unsafe extern "C" fn savepoint(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        let _span = tracing::debug_span!("savepoint").entered();
        match table.policy {
            Policy::Flush => table.flush(),
            Policy::Mark => {
                if table.pending.mark(index) {
                    tracing::warn!(marks = table.pending.marks(), "{PENDING_MARK_DIAGNOSTIC}");
                    table.pending.flush_through_marks();
                    return table.flush();
                }
                Ok(())
            }
        }
    })
}

unsafe extern "C" fn release(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        let _span = tracing::debug_span!("release").entered();
        match table.policy {
            Policy::Flush => table.flush(),
            Policy::Mark => {
                table.pending.release(index);
                Ok(())
            }
        }
    })
}

// Everything staged before the savepoint is in the arrangement under Flush and
// SQLite unwinds those pages itself, so discarding the buffer is the whole job.
unsafe extern "C" fn rollback_to(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        let _span = tracing::debug_span!("rollback_to/discard").entered();
        match table.policy {
            Policy::Flush => {
                table.pending.discard();
                Ok(())
            }
            Policy::Mark => table.pending.rewind(index).map_err(|reason| {
                let _span = tracing::debug_span!("rollback_to/spilled").entered();
                error(reason)
            }),
        }
    })
}

#[repr(C)]
pub struct Cursor {
    base: ffi::sqlite3_vtab_cursor,
    db: *mut ffi::sqlite3,
    state: String,
    rows: Vec<(i64, i64, i64)>,
    at: usize,
}

unsafe impl VTabCursor for Cursor {
    fn filter(&mut self, _: c_int, _: Option<&str>, _: &Filters<'_>) -> Result<()> {
        let db = unsafe { Connection::from_handle(self.db)? };
        self.rows = db
            .prepare(&format!(
                "SELECT g,n,s FROM main.{} ORDER BY g",
                self.state
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>>>()?;
        self.at = 0;
        Ok(())
    }
    fn next(&mut self) -> Result<()> {
        self.at += 1;
        Ok(())
    }
    fn eof(&self) -> bool {
        self.at >= self.rows.len()
    }
    fn column(&self, ctx: &mut Context, i: c_int) -> Result<()> {
        let row = self.rows[self.at];
        match i {
            0 => ctx.set_result(&row.0),
            1 => ctx.set_result(&row.1),
            2 => ctx.set_result(&row.2),
            _ => ctx.set_result(&rusqlite::types::Null),
        }
    }
    fn rowid(&self) -> Result<i64> {
        Ok(self.at as i64 + 1)
    }
}
