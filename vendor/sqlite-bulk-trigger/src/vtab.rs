use crate::collector::{
    BulkTrigger, Collector, Counts, RowChange, Sign, STAGED_BYTES, STAGED_ROWS,
};
use crate::schema::{
    self, error, FIRST_VALUE_COLUMN, ROWID_ARGUMENTS, SIGN_COLUMN, SOURCE_COLUMN,
};
use rusqlite::{ffi, types::Value, types::ValueRef, vtab::*, Connection, Result};
use std::{
    borrow::Cow,
    cell::{RefCell, RefMut},
    collections::HashMap,
    ffi::{c_int, CStr, CString},
    rc::Rc,
};

/// The state machine plus what only the standalone path needs: the consumer's
/// callback and the column count of each watched table.
struct Installed {
    collector: Collector,
    /// Taken out for the duration of `on_batch`, so a statement that re-enters
    /// the collector from inside the callback finds no callback to run.
    trigger: Option<Box<dyn BulkTrigger>>,
    arity: HashMap<String, usize>,
}

/// A vtab is disconnected and reconnected whenever SQLite resets the schema,
/// so the batch outlives the `Table` and lives here, keyed by connection.
type Key = (usize, String);

thread_local! {
    static COLLECTORS: RefCell<HashMap<Key, Rc<RefCell<Installed>>>> =
        RefCell::new(HashMap::new());
}

fn key(db: *mut ffi::sqlite3, name: &str) -> Key {
    (db as usize, name.to_string())
}

fn lookup(db: *mut ffi::sqlite3, name: &str) -> Option<Rc<RefCell<Installed>>> {
    COLLECTORS.with(|map| map.borrow().get(&key(db, name)).cloned())
}

fn state_mut(state: &Rc<RefCell<Installed>>) -> Result<RefMut<'_, Installed>> {
    state
        .try_borrow_mut()
        .map_err(|_| error("the collector re-entered a callback while its state was borrowed"))
}

/// rusqlite's trampolines do not catch unwinding, and an unwind across the C
/// frame is undefined, so every collector body runs inside this.
fn guarded<R>(what: &'static str, body: impl FnOnce() -> Result<R>) -> Result<R> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(error(format!("panic inside the collector {what} callback"))))
}

/// Installs a collector named `name` over `tables` with the default caps.
pub fn watch<T: BulkTrigger>(
    db: &Connection,
    name: &str,
    tables: &[&str],
    trigger: T,
) -> Result<()> {
    Watch::new(name).tables(tables).install(db, trigger)
}

/// Builder form of [`watch`], for tuning the two spill caps.
pub struct Watch<'a> {
    name: &'a str,
    tables: Vec<&'a str>,
    staged_rows: usize,
    staged_bytes: usize,
}

impl<'a> Watch<'a> {
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
            tables: Vec::new(),
            staged_rows: STAGED_ROWS,
            staged_bytes: STAGED_BYTES,
        }
    }

    pub fn tables(mut self, tables: &[&'a str]) -> Self {
        self.tables.extend_from_slice(tables);
        self
    }

    pub fn staged_rows(mut self, rows: usize) -> Self {
        self.staged_rows = rows;
        self
    }

    pub fn staged_bytes(mut self, bytes: usize) -> Self {
        self.staged_bytes = bytes;
        self
    }

    /// Registers the module, creates the collector table, its shadow table and
    /// three triggers per watched table, then zeroes the callback counts.
    pub fn install<T: BulkTrigger>(self, db: &Connection, trigger: T) -> Result<()> {
        const MODULE: Module<Table> = unsafe {
            let mut raw: ffi::sqlite3_module =
                std::mem::transmute(Module::<Table>::update_module_with_tx());
            raw.iVersion = 2;
            raw.xSavepoint = Some(savepoint);
            raw.xRelease = Some(release);
            raw.xRollbackTo = Some(rollback_to);
            std::mem::transmute(raw)
        };
        schema::check_identifier(self.name)?;
        if self.tables.is_empty() {
            return Err(error("a collector needs at least one watched table"));
        }
        let handle = unsafe { db.handle() };
        if lookup(handle, self.name).is_some() {
            return Err(error(format!(
                "collector {:?} is already installed",
                self.name
            )));
        }
        let mut arity = HashMap::new();
        let mut layout = Vec::new();
        for table in &self.tables {
            let columns = schema::columns(db, table)?;
            arity.insert((*table).to_string(), columns.len());
            layout.push(((*table).to_string(), columns));
        }
        let width = arity.values().copied().max().unwrap_or(0);
        let state = Rc::new(RefCell::new(Installed {
            collector: Collector::new(self.name, width)
                .staged_rows(self.staged_rows)
                .staged_bytes(self.staged_bytes),
            trigger: Some(Box::new(trigger)),
            arity,
        }));
        db.create_module(self.name, &MODULE, None::<()>)?;
        COLLECTORS.with(|map| map.borrow_mut().insert(key(handle, self.name), state.clone()));
        let built = self.build(db, &layout);
        if built.is_err() {
            COLLECTORS.with(|map| map.borrow_mut().remove(&key(handle, self.name)));
        }
        built?;
        state_mut(&state)?.collector.reset_counts();
        Ok(())
    }

    fn build(&self, db: &Connection, layout: &[(String, Vec<String>)]) -> Result<()> {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE main.{} USING {}",
            schema::quote(self.name),
            self.name
        ))?;
        for (table, columns) in layout {
            db.execute_batch(&schema::create_triggers(self.name, table, columns))?;
        }
        Ok(())
    }
}

/// Callback counts for the collector named `name` on this connection.
pub fn counts(db: &Connection, name: &str) -> Option<Counts> {
    let state = lookup(unsafe { db.handle() }, name)?;
    let counts = state.try_borrow().ok()?.collector.counts();
    Some(counts)
}

#[repr(C)]
pub struct Table {
    base: ffi::sqlite3_vtab,
    db: Connection, // Non-owning handle, valid for this virtual-table connection.
    name: String,
    width: usize,
    handle: *mut ffi::sqlite3,
    state: Rc<RefCell<Installed>>,
}

impl Table {
    fn attach(
        db: &mut VTabConnection,
        database: &[u8],
        name: &[u8],
        create: bool,
    ) -> Result<(Cow<'static, CStr>, Self)> {
        if database != b"main" {
            return Err(error("a collector table must be in main"));
        }
        let name = std::str::from_utf8(name)
            .map_err(|e| error(e.to_string()))?
            .to_string();
        let handle = unsafe { db.handle() };
        let state = lookup(handle, &name)
            .ok_or_else(|| error(format!("collector {name:?} was not installed by watch()")))?;
        let connection = unsafe { Connection::from_handle(handle)? };
        let width = {
            let installed = state.try_borrow().map_err(|_| error("collector busy"))?;
            if create {
                installed.collector.create_shadow(&connection)?;
            }
            installed.arity.values().copied().max().unwrap_or(0)
        };
        let declaration =
            CString::new(schema::declaration(width)).map_err(|e| error(e.to_string()))?;
        Ok((
            Cow::Owned(declaration),
            Self {
                base: ffi::sqlite3_vtab::default(),
                db: connection,
                name,
                width,
                handle,
                state,
            },
        ))
    }

    fn flush(&self) -> Result<()> {
        let (batch, taken) = {
            let mut installed = state_mut(&self.state)?;
            let batch = installed.collector.drain(&self.db)?;
            if batch.is_empty() {
                return Ok(());
            }
            let taken = installed.trigger.take();
            (batch, taken)
        };
        let Some(mut trigger) = taken else {
            return Err(error("the collector callback re-entered its own flush"));
        };
        let delivered = trigger.on_batch(&self.db, &batch);
        match state_mut(&self.state) {
            Ok(mut installed) => installed.trigger = Some(trigger),
            Err(reason) => tracing::error!(%reason, "the collector could not take its trigger back"),
        }
        delivered
    }
}

unsafe impl<'vtab> VTab<'vtab> for Table {
    type Aux = ();
    type Cursor = Cursor;

    fn connect(
        db: &mut VTabConnection,
        _: Option<&()>,
        _: &[u8],
        database: &[u8],
        name: &[u8],
        _: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        Self::attach(db, database, name, false)
    }

    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        info.set_estimated_cost(1.0);
        info.set_estimated_rows(1);
        Ok(true)
    }

    fn open(&'vtab mut self) -> Result<Cursor> {
        let staged_rows = self
            .state
            .try_borrow()
            .map(|installed| installed.collector.staged().len() as i64)
            .unwrap_or(-1);
        Ok(Cursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            staged_rows,
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
        database: &[u8],
        name: &[u8],
        _: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        Self::attach(db, database, name, true)
    }

    fn destroy(&self) -> Result<()> {
        let installed = self
            .state
            .try_borrow()
            .map_err(|_| error("a collector cannot be dropped from inside a callback"))?;
        let watched = installed.arity.keys().cloned().collect::<Vec<_>>();
        self.db
            .execute_batch(&schema::drop_triggers(&self.name, &watched))?;
        installed.collector.drop_shadow(&self.db)?;
        drop(installed);
        COLLECTORS.with(|map| map.borrow_mut().remove(&key(self.handle, &self.name)));
        Ok(())
    }
}

impl<'vtab> UpdateVTab<'vtab> for Table {
    fn delete(&mut self, _: ValueRef<'_>) -> Result<()> {
        Err(error("a collector table is written by its triggers only"))
    }

    fn update(&mut self, _: &Updates<'_>) -> Result<()> {
        Err(error("a collector table is written by its triggers only"))
    }

    fn insert(&mut self, args: &Inserts<'_>) -> Result<i64> {
        guarded("xUpdate", || {
            if args.len() != ROWID_ARGUMENTS + FIRST_VALUE_COLUMN + self.width {
                return Err(error("a collector table is written by its triggers only"));
            }
            let table: String = args.get(ROWID_ARGUMENTS + SOURCE_COLUMN)?;
            let code: i64 = args.get(ROWID_ARGUMENTS + SIGN_COLUMN)?;
            let sign = Sign::from_integer(code)
                .ok_or_else(|| error(format!("a trigger wrote sign {code}")))?;
            let mut installed = state_mut(&self.state)?;
            let arity = *installed
                .arity
                .get(&table)
                .ok_or_else(|| error(format!("{table:?} is not watched here")))?;
            let values = (0..arity)
                .map(|at| args.get::<Value>(ROWID_ARGUMENTS + FIRST_VALUE_COLUMN + at))
                .collect::<Result<Vec<_>>>()?;
            installed
                .collector
                .update(&self.db, RowChange::new(table, sign, values))?;
            Ok(0)
        })
    }
}

impl<'vtab> TransactionVTab<'vtab> for Table {
    fn begin(&mut self) -> Result<()> {
        guarded("xBegin", || {
            state_mut(&self.state)?.collector.begin();
            Ok(())
        })
    }

    fn sync(&mut self) -> Result<()> {
        guarded("xSync", || self.flush())
    }

    fn commit(&mut self) -> Result<()> {
        guarded("xCommit", || {
            state_mut(&self.state)?.collector.commit();
            Ok(())
        })
    }

    fn rollback(&mut self) -> Result<()> {
        guarded("xRollback", || {
            state_mut(&self.state)?.collector.rollback();
            Ok(())
        })
    }
}

fn dispatch(
    raw: *mut ffi::sqlite3_vtab,
    what: &'static str,
    body: impl FnOnce(&mut Table) -> Result<()>,
) -> c_int {
    let outcome = guarded(what, || {
        let table = unsafe { &mut *raw.cast::<Table>() };
        body(table)
    });
    match outcome {
        Ok(()) => ffi::SQLITE_OK,
        Err(reason) => unsafe { rusqlite::to_sqlite_error(&reason, &mut (*raw).zErrMsg) },
    }
}

unsafe extern "C" fn savepoint(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, "xSavepoint", |table| {
        state_mut(&table.state)?.collector.savepoint(index);
        Ok(())
    })
}

unsafe extern "C" fn release(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, "xRelease", |table| {
        state_mut(&table.state)?.collector.release(index);
        Ok(())
    })
}

/// The rows this rewinds past are inside SQLite's own transaction, so the pager
/// already removed the spilled ones before this callback ran.
unsafe extern "C" fn rollback_to(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, "xRollbackTo", |table| {
        state_mut(&table.state)?.collector.rollback_to(index);
        Ok(())
    })
}

#[repr(C)]
pub struct Cursor {
    base: ffi::sqlite3_vtab_cursor,
    staged_rows: i64,
    at: usize,
}

unsafe impl VTabCursor for Cursor {
    fn filter(&mut self, _: c_int, _: Option<&str>, _: &Filters<'_>) -> Result<()> {
        self.at = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.at += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.at >= 1
    }

    fn column(&self, ctx: &mut Context, index: c_int) -> Result<()> {
        match index {
            0 => ctx.set_result(&self.staged_rows),
            _ => ctx.set_result(&rusqlite::types::Null),
        }
    }

    fn rowid(&self) -> Result<i64> {
        Ok(1)
    }
}
