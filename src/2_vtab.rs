use crate::{
    catalog::{self, error, quote},
    statements::{self, Phase},
    relational_maintenance,
    relational_program::Program,
};
use rusqlite::{ffi, types::ValueRef, vtab::*, Connection, Result};
use std::{
    borrow::Cow,
    ffi::{c_char, c_int, CStr, CString},
    ptr,
};

// rusqlite 0.40.2 documents Module as repr(transparent) over sqlite3_module.
// Keep its allocation, cursor, update and error adapters; fill the two missing
// SQLite callbacks in the ABI descriptor. No C source or replacement binding.
pub fn register(db: &Connection) -> Result<()> {
    const MODULE: Module<Table> = unsafe {
        let mut raw: ffi::sqlite3_module =
            std::mem::transmute(Module::<Table>::update_module_with_tx());
        raw.iVersion = 3;
        raw.xRename = Some(rename);
        raw.xShadowName = Some(shadow_name);
        raw.xSavepoint = Some(savepoint);
        raw.xRelease = Some(release);
        raw.xRollbackTo = Some(rollback_to);
        std::mem::transmute(raw)
    };
    db.create_module(c"sqlite_ivm", &MODULE, None::<()>)
}

#[repr(C)]
struct Table {
    base: ffi::sqlite3_vtab,
    db: Connection, // Non-owning handle, valid for this virtual-table connection.
    id: i64,        // Explicit INTEGER PRIMARY KEY survives VACUUM; names are never cached.
    sql: String,
    /// Source DDL generation `sql` was read at; `refresh` reads the catalog again only after a rewrite.
    generation: u64,
    roles: Vec<c_int>,
    plan: Option<crate::relational::Plan>,
    /// Every SQL string this view's drain issues, built beside the plan.
    program: Option<Program>,
    /// Source rows staged since the last drain. Present once the plan is bound.
    collector: Option<sqlite_bulk_trigger::Collector>,
}

fn recursive(plan: &crate::relational::Plan) -> bool {
    plan.nodes
        .iter()
        .any(|n| matches!(n.kind, crate::relational::Kind::Fixpoint { .. }))
}
fn declaration(plan: &crate::relational::Plan) -> String {
    let arity = plan.sources.iter().map(|s| s.columns.len()).max().unwrap_or(0);
    let hidden = (0..arity)
        .map(|i| format!("__ivm_v{i} HIDDEN"))
        .collect::<Vec<_>>()
        .join(",");
    format!("CREATE TABLE x({},{},{})",
    plan.names.iter().zip(&plan.nodes[plan.output].fields).map(|(name,f)|format!("{} {} COLLATE {}",quote(name),f.affinity,f.collation)).collect::<Vec<_>>().join(","),
    "__ivm_source INTEGER HIDDEN,__ivm_adding INTEGER HIDDEN",
    hidden)
}
// Storage formats before this pass: `format_version<5` predates AUTOINCREMENT
// fixpoint members; `generic=0` rows carry the retired per-row engine's
// `_state(g,n,s)`/`_delta` tables and source key indexes. Both rebind here,
// inside xConnect, before any trigger of the stored hooks can run.
pub(crate) fn migrate(
    conn: &Connection,
    name: &str,
    legacy: bool,
    format: i64,
) -> Result<Option<String>> {
    let sql: String = statements::query(
        conn,
        Phase::Declare,
        name,
        "SELECT query_sql FROM main.__ivm_views WHERE name=?1",
        [name],
        |r| r.get(0),
    )?;
    let plan = crate::relational::bind(conn, &sql)?;
    if format < 5 && recursive(&plan) {
        return Ok(None);
    }
    drop_triggers(conn, name)?;
    if legacy {
        convert(conn, name, &plan)?;
    } else {
        relational_maintenance::hooks(conn, name, &plan)?;
        statements::exec(conn,Phase::Declare,name,"UPDATE main.__ivm_objects SET definition=(SELECT sql FROM main.sqlite_schema WHERE type=object_type AND name=object_name) WHERE view_name=?1",[name])?;
        set_schema(conn, name, &plan)?;
    }
    Ok(Some(declaration(&plan)))
}
// A `generic=0` row stores the retired engine's shadows. Every entry that can
// meet one rebuilds it as a relational arrangement: xConnect through `migrate`,
// source DDL directly, with the already-rewritten SQL.
pub(crate) fn convert(
    conn: &Connection,
    name: &str,
    plan: &crate::relational::Plan,
) -> Result<()> {
    let retired: Vec<(String, String)> = statements::query_map(
        conn,
        Phase::Declare,
        name,
        "SELECT object_type,object_name FROM main.__ivm_objects WHERE view_name=?1 AND object_type IN ('table','index')",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for (kind, object) in &retired {
        statements::batch(conn, Phase::Declare, name, &format!("DROP {kind} main.{}", quote(object)))?;
    }
    let mut objects = plan.create_state(conn, name)?;
    let collector = plan.collector(name);
    statements::guard(
        Phase::Declare,
        name,
        &format!("CREATE TABLE {}", collector.shadow_table()),
        || collector.create_shadow(conn),
    )?;
    objects.push(("table", collector.shadow_table()));
    objects.extend(relational_maintenance::hooks(conn, name, plan)?);
    statements::exec(
        conn,
        Phase::Declare,
        name,
        "DELETE FROM main.__ivm_objects WHERE view_name=?1",
        [name],
    )?;
    for (kind, object) in &objects {
        let definition: String = statements::query(
            conn,
            Phase::Declare,
            name,
            "SELECT coalesce(sql,'') FROM main.sqlite_schema WHERE type=?1 AND name=?2",
            rusqlite::params![kind, object],
            |r| r.get(0),
        )?;
        statements::exec(
            conn,
            Phase::Declare,
            name,
            "INSERT INTO main.__ivm_objects VALUES(?1,?2,?3,?4)",
            rusqlite::params![name, kind, object, definition],
        )?;
    }
    plan.populate(conn, name)?;
    set_schema(conn, name, plan)?;
    Ok(())
}
pub(crate) fn drop_triggers(conn: &Connection, name: &str) -> Result<()> {
    let triggers: Vec<String> = statements::query_map(
        conn,
        Phase::Teardown,
        name,
        "SELECT object_name FROM main.__ivm_objects WHERE view_name=?1 AND object_type='trigger'",
        [name],
        |r| r.get(0),
    )?;
    for trigger in &triggers {
        statements::batch(
            conn,
            Phase::Teardown,
            name,
            &format!("DROP TRIGGER main.{}", quote(trigger)),
        )?;
    }
    Ok(())
}
fn set_schema(conn: &Connection, name: &str, plan: &crate::relational::Plan) -> Result<()> {
    let roles = (1..=plan.names.len())
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    statements::exec(
        conn,
        Phase::Declare,
        name,
        "UPDATE main.__ivm_schema SET declaration=?1, roles=?2, generic=1, format_version=5 WHERE id=(SELECT id FROM main.__ivm_views WHERE name=?3)",
        rusqlite::params![declaration(plan), roles, name],
    )?;
    Ok(())
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|e| error(e.to_string()))
}
fn query_argument(args: &[&[u8]]) -> Result<String> {
    if args.len() != 1 {
        return Err(error(
            "sqlite_ivm requires one single-quoted SELECT argument",
        ));
    }
    let value = text(args[0])?.trim();
    if !value.starts_with('\'') || !value.ends_with('\'') || value.len() < 2 {
        return Err(error(
            "sqlite_ivm requires one single-quoted SELECT argument",
        ));
    }
    let inner = &value[1..value.len() - 1];
    // Only SQL's doubled single-quote escape is accepted.
    let mut chars = inner.chars();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\'' && chars.next() != Some('\'') {
            return Err(error("invalid quoted query"));
        }
        result.push(ch);
    }
    Ok(result)
}
impl Table {
    fn name(&self) -> Result<String> {
        statements::query_cached(
            &self.db,
            Phase::Declare,
            "catalog",
            "SELECT name FROM main.__ivm_views WHERE id=?1",
            [self.id],
            |r| r.get(0),
        )
    }
    fn attach(
        db: &mut VTabConnection,
        schema: &[u8],
        name: &[u8],
        args: &[&[u8]],
        create: bool,
    ) -> Result<(Cow<'static, CStr>, Self)> {
        if schema != b"main" {
            return Err(error("sqlite_ivm tables must be in main"));
        }
        let name = text(name)?;
        let conn = unsafe { Connection::from_handle(db.handle())? };
        conn.set_prepared_statement_cache_capacity(8192);
        let stored: bool = statements::query(
            &conn,
            Phase::Declare,
            name,
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_schema')",
            [],
            |r| r.get(0),
        )?;
        if stored {
            let versioned:bool=statements::query(&conn,Phase::Declare,name,"SELECT EXISTS(SELECT 1 FROM pragma_table_info('__ivm_schema','main') WHERE name='format_version')",[],|r|r.get(0))?;
            if !versioned {
                return Err(error("sqlite_ivm storage format 1 requires the matching older extension; automatic migration is unavailable"));
            }
            let incompatible: bool = statements::query(
                &conn,
                Phase::Declare,
                name,
                "SELECT EXISTS(SELECT 1 FROM main.__ivm_schema WHERE format_version NOT IN (2,3,4,5))",
                [],
                |r| r.get(0),
            )?;
            if incompatible {
                return Err(error("incompatible sqlite_ivm storage format"));
            }
        }
        let sql = if create {
            query_argument(args)?
        } else {
            statements::query(
                &conn,
                Phase::Declare,
                name,
                "SELECT query_sql FROM main.__ivm_views WHERE name=?1",
                [name],
                |r| r.get(0),
            )?
        };
        if !create {
            let (id,declaration,generic,roles,format):(i64,String,bool,String,i64)=statements::query(&conn,Phase::Declare,name,"SELECT v.id,s.declaration,s.generic,s.roles,s.format_version FROM main.__ivm_views v JOIN main.__ivm_schema s ON s.id=v.id WHERE v.name=?1",[name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            let mut roles = roles
                .split(',')
                .map(|s| {
                    s.parse::<c_int>()
                        .map_err(|_| error("invalid stored schema"))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut declaration = declaration;
            if format < 5 || !generic {
                match migrate(&conn, name, !generic, format) {
                    Ok(Some(fresh)) => {
                        declaration = fresh;
                        roles = statements::query(
                                &conn,
                                Phase::Declare,
                                name,
                                "SELECT s.roles FROM main.__ivm_schema s JOIN main.__ivm_views v ON v.id=s.id WHERE v.name=?1",
                                [name],
                                |r| {
                                    r.get::<_, String>(0)?
                                        .split(',')
                                        .map(|p| p.parse::<c_int>().map_err(|_| error("invalid stored schema")))
                                        .collect::<Result<Vec<_>>>()
                                },
                            )?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let readonly = matches!(
                            &e,
                            rusqlite::Error::SqliteFailure(f, _)
                                if f.code == rusqlite::ErrorCode::ReadOnly
                        );
                        if readonly {
                            return Err(error(
                                "storage format migration requires a writable database",
                            ));
                        }
                        return Err(e);
                    }
                }
            }
            let mut table = Self {
                base: ffi::sqlite3_vtab::default(),
                db: conn,
                id,
                sql: String::new(),
                generation: u64::MAX, // Never the live value, so the refresh below reads.
                roles,
                plan: None,
                program: None,
                collector: None,
            };
            // Bind now so the scratch tables exist before any trigger program
            // runs; a failure here surfaces again at the first write.
            let _ = table.refresh();
            return Ok((
                Cow::Owned(CString::new(declaration).map_err(|e| error(e.to_string()))?),
                table,
            ));
        }
        let plan = crate::relational::bind(&conn, &sql)?;
        relational_maintenance::install(&conn, name, &sql, &plan)?;
        let program = Program::build(&plan, name, &conn);
        plan.prepare_scratch(&conn, &program)?;
        let id = statements::query(
            &conn,
            Phase::Declare,
            name,
            "SELECT id FROM main.__ivm_views WHERE name=?1",
            [name],
            |r| r.get(0),
        )?;
        let declaration = declaration(&plan);
        let roles: Vec<c_int> = (1..=plan.names.len() as c_int).collect();
        statements::batch(&conn,Phase::Declare,name,"CREATE TABLE IF NOT EXISTS main.__ivm_schema(id INTEGER PRIMARY KEY,declaration TEXT NOT NULL,generic INTEGER NOT NULL,roles TEXT NOT NULL,format_version INTEGER NOT NULL)")?;
        // Format 5: fixpoint member tables carry AUTOINCREMENT rowids, so a
        // drain can mark new members by rowid after deletes.
        statements::exec(
            &conn,
            Phase::Declare,
            name,
            "INSERT INTO main.__ivm_schema VALUES(?1,?2,1,?3,5)",
            rusqlite::params![
                id,
                declaration,
                roles
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ],
        )?;
        Ok((
            Cow::Owned(CString::new(declaration).map_err(|e| error(e.to_string()))?),
            Self {
                base: ffi::sqlite3_vtab::default(),
                db: conn,
                id,
                sql,
                generation: crate::source_ddl::generation(),
                roles,
                collector: Some(plan.collector(name)),
                plan: Some(plan),
                program: Some(program),
            },
        ))
    }
    fn refresh(&mut self) -> Result<()> {
        let generation = crate::source_ddl::generation();
        if generation == self.generation {
            return Ok(());
        }
        let sql: String = statements::query_cached(
            &self.db,
            Phase::Declare,
            "catalog",
            "SELECT query_sql FROM main.__ivm_views WHERE id=?1",
            [self.id],
            |r| r.get(0),
        )?;
        if sql != self.sql {
            let plan = crate::relational::bind(&self.db, &sql)?;
            let format: i64 = statements::query(
                &self.db,
                Phase::Declare,
                "catalog",
                "SELECT format_version FROM main.__ivm_schema WHERE id=?1",
                [self.id],
                |r| r.get(0),
            )?;
            if format < 5 && recursive(&plan) {
                return Err(error(
                    "recursive views from storage formats before 5 must be dropped and re-created",
                ));
            }
            let name = self.name()?;
            let program = Program::build(&plan, &name, &self.db);
            plan.prepare_scratch(&self.db, &program)?;
            self.collector = Some(plan.collector(&name));
            self.plan = Some(plan);
            self.program = Some(program);
            self.sql = sql;
        }
        // Stamped after the bind: a failed connect-time bind must run again.
        self.generation = generation;
        Ok(())
    }
    fn rename_to(&self, new: &str) -> Result<()> {
        relational_maintenance::validate_name(new)?;
        let old = self.name()?;
        let objects = catalog::manifest(&self.db, &old, Some(new))?;
        let mut renamed = Vec::new();
        // Source trigger bodies are regenerated. Index B-trees retain their names:
        // SQLite disallows DROP INDEX while the outer ALTER statement is active.
        // SQLite calls xRename with legacy_alter_table enabled.
        for (kind, name, _) in &objects {
            if kind == "index" {
                renamed.push(("index", name.clone()));
            }
            if kind == "trigger" {
                statements::batch(
                    &self.db,
                    Phase::Declare,
                    new,
                    &format!("DROP {} main.{}", kind.to_uppercase(), quote(name)),
                )
                .map_err(|e| error(format!("rename dropping {kind} {name}: {e}")))?;
            }
        }
        for (kind, name, _) in &objects {
            if kind == "table" {
                let suffix = name
                    .strip_prefix(&format!("{old}_"))
                    .ok_or_else(|| error("invalid shadow table name"))?;
                let target = format!("{new}_{suffix}");
                statements::batch(
                    &self.db,
                    Phase::Declare,
                    new,
                    &format!(
                        "ALTER TABLE main.{} RENAME TO {}",
                        quote(name),
                        quote(&target)
                    ),
                )
                .map_err(|e| error(format!("rename shadow {name}: {e}")))?;
                renamed.push(("table", target));
            }
        }
        renamed.extend(relational_maintenance::hooks(
            &self.db,
            new,
            self.plan.as_ref().ok_or_else(|| error("plan missing after bind"))?,
        )?);
        statements::exec(
            &self.db,
            Phase::Declare,
            new,
            "DELETE FROM main.__ivm_objects WHERE view_name=?1",
            [&old],
        )?;
        for table in ["__ivm_sources", "__ivm_columns"] {
            statements::exec(
                &self.db,
                Phase::Declare,
                new,
                &format!("UPDATE main.{table} SET view_name=?1 WHERE view_name=?2"),
                [new, &old],
            )?;
        }
        statements::exec(
            &self.db,
            Phase::Declare,
            new,
            "UPDATE main.__ivm_views SET name=?1 WHERE id=?2",
            rusqlite::params![new, self.id],
        )?;
        for (kind, name) in renamed {
            let ddl: String = statements::query(
                &self.db,
                Phase::Declare,
                new,
                "SELECT sql FROM main.sqlite_schema WHERE type=?1 AND name=?2",
                [kind, &name],
                |r| r.get(0),
            )?;
            statements::exec(
                &self.db,
                Phase::Declare,
                new,
                "INSERT INTO main.__ivm_objects VALUES(?1,?2,?3,?4)",
                [new, kind, &name, &ddl],
            )?;
        }
        Ok(())
    }
}
impl Table {
    /// Hands every staged source row to the plan in one batch.
    fn drain(&mut self) -> Result<()> {
        let Some(collector) = self.collector.as_mut() else {
            return Ok(());
        };
        if collector.staged().is_empty() && collector.spilled_rows() == 0 {
            return Ok(());
        }
        let shadow = collector.shadow_table();
        let batch = statements::guard(
            Phase::Drain,
            &shadow,
            &format!("DELETE FROM {shadow} RETURNING *"),
            || collector.drain(&self.db),
        )?;
        let name = self.name()?;
        let stale = self
            .program
            .as_ref()
            .map(|program| program.name != name)
            .unwrap_or(true);
        if stale {
            let plan = self.plan.as_ref().ok_or_else(|| error("plan missing after bind"))?;
            let program = Program::build(plan, &name, &self.db);
            self.program = Some(program);
        }
        let batch = batch
            .into_iter()
            .map(|change| {
                let source: usize = change
                    .table
                    .parse()
                    .map_err(|_| error("invalid source ordinal"))?;
                let sign = match change.sign {
                    sqlite_bulk_trigger::Sign::Insert => 1,
                    sqlite_bulk_trigger::Sign::Delete => -1,
                };
                Ok((source, change.values, sign))
            })
            .collect::<Result<Vec<_>>>()?;
        let program = self
            .program
            .as_ref()
            .ok_or_else(|| error("program missing after bind"))?;
        let plan = self.plan.as_ref().ok_or_else(|| error("plan missing after bind"))?;
        plan.drain(&self.db, program, &batch)
    }
}
impl<'vtab> TransactionVTab<'vtab> for Table {
    fn begin(&mut self) -> Result<()> {
        if let Some(collector) = self.collector.as_mut() {
            collector.begin();
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<()> {
        self.drain()
    }
    fn commit(&mut self) -> Result<()> {
        if let Some(collector) = self.collector.as_mut() {
            collector.commit();
        }
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        if let Some(collector) = self.collector.as_mut() {
            collector.rollback();
        }
        Ok(())
    }
}
fn dispatch(raw: *mut ffi::sqlite3_vtab, body: impl FnOnce(&mut Table)) -> c_int {
    // Never unwind into C.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { &mut *raw.cast::<Table>() };
        body(table)
    }));
    match result {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => unsafe {
            rusqlite::to_sqlite_error(
                &error("panic in virtual-table savepoint callback"),
                &mut (*raw).zErrMsg,
            )
        },
    }
}
unsafe extern "C" fn savepoint(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        if let Some(collector) = table.collector.as_mut() {
            collector.savepoint(index);
        }
    })
}
unsafe extern "C" fn release(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        if let Some(collector) = table.collector.as_mut() {
            collector.release(index);
        }
    })
}
unsafe extern "C" fn rollback_to(raw: *mut ffi::sqlite3_vtab, index: c_int) -> c_int {
    dispatch(raw, |table| {
        if let Some(collector) = table.collector.as_mut() {
            collector.rollback_to(index);
        }
    })
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
        Self::attach(db, schema, name, args, false)
    }
    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        info.set_estimated_cost(1_000_000.0);
        Ok(true)
    }
    fn open(&'vtab mut self) -> Result<Cursor> {
        // Read-your-writes: a read inside the transaction drains first.
        self.drain()?;
        Ok(Cursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            db: unsafe { self.db.handle() },
            state: format!("{}_state", self.name()?),
            roles: self.roles.clone(),
            statement: ptr::null_mut(),
            statements: None,
            done: true,
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
        Self::attach(db, schema, name, args, true)
    }
    fn destroy(&self) -> Result<()> {
        catalog::uninstall(&self.db, &self.name()?)
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
        self.refresh()?;
        let count = self.roles.len();
        let arity = self
            .plan
            .as_ref()
            .ok_or_else(|| error("plan missing after bind"))?
            .sources
            .iter()
            .map(|s| s.columns.len())
            .max()
            .unwrap_or(0);
        if args.len() != count + 4 + arity
            || args.iter().take(count + 2).any(|v| v != ValueRef::Null)
        {
            return Err(error("managed results are read-only"));
        }
        let source = usize::try_from(args.get::<i64>(count + 2)?)
            .map_err(|_| error("invalid source ordinal"))?;
        let adding: i64 = args.get(count + 3)?;
        if !(0..=1).contains(&adding) {
            return Err(error("invalid maintenance command"));
        }
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| error("plan missing after bind"))?;
        let src = plan
            .sources
            .get(source)
            .ok_or_else(|| error("invalid source ordinal"))?;
        let row: Vec<rusqlite::types::Value> = (0..src.columns.len())
            .map(|i| args.get(count + 4 + i))
            .collect::<Result<Vec<_>>>()?;
        plan.validate(source, &row)?;
        let sign = if adding == 1 {
            sqlite_bulk_trigger::Sign::Insert
        } else {
            sqlite_bulk_trigger::Sign::Delete
        };
        let collector = self
            .collector
            .as_mut()
            .ok_or_else(|| error("collector missing after bind"))?;
        let shadow = collector.shadow_table();
        statements::guard(
            Phase::Drain,
            &shadow,
            &format!("INSERT INTO {shadow} VALUES(...)"),
            || {
                collector.update(
                    &self.db,
                    sqlite_bulk_trigger::RowChange::new(source.to_string(), sign, row),
                )
            },
        )?;
        Ok(0)
    }
}

#[repr(C)]
struct Cursor {
    base: ffi::sqlite3_vtab_cursor,
    db: *mut ffi::sqlite3,
    state: String,
    roles: Vec<c_int>,
    statement: *mut ffi::sqlite3_stmt,
    /// The statement span for this cursor's result read, entered on every step so
    /// the final step's profile event nests under it.
    statements: Option<statements::Statement>,
    done: bool,
}
impl Cursor {
    fn check(&self, rc: c_int) -> Result<()> {
        if rc == ffi::SQLITE_OK {
            return Ok(());
        }
        Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(rc),
            Some(
                unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.db)) }
                    .to_string_lossy()
                    .into_owned(),
            ),
        ))
    }
}
impl Drop for Cursor {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_finalize(self.statement);
        }
    }
}
unsafe impl VTabCursor for Cursor {
    fn filter(&mut self, _: c_int, _: Option<&str>, _: &Filters<'_>) -> Result<()> {
        unsafe {
            ffi::sqlite3_finalize(self.statement);
        }
        self.statement = ptr::null_mut();
        self.done = true;
        let select = format!(
            "SELECT rowid,{} FROM main.{}",
            (0..self.roles.len())
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>()
                .join(","),
            quote(&self.state)
        );
        self.statements = Some(statements::open(
            Phase::Materialize,
            &self.state,
            &select,
            statements::FRESH,
        ));
        let sql = CString::new(select).map_err(|e| error(e.to_string()))?;
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                self.db,
                sql.as_ptr(),
                -1,
                &mut self.statement,
                ptr::null_mut(),
            )
        };
        self.check(rc)?;
        self.next()
    }
    fn next(&mut self) -> Result<()> {
        let _statements = self.statements.as_ref().map(|statement| statement.enter());
        let rc = unsafe { ffi::sqlite3_step(self.statement) };
        self.done = rc != ffi::SQLITE_ROW;
        if rc == ffi::SQLITE_ROW || rc == ffi::SQLITE_DONE {
            Ok(())
        } else {
            self.check(rc)
        }
    }
    fn eof(&self) -> bool {
        self.done
    }
    fn column(&self, ctx: &mut Context, i: c_int) -> Result<()> {
        if let Some(role) = self.roles.get(i as usize) {
            let value = unsafe {
                match ffi::sqlite3_column_type(self.statement, *role) {
                    ffi::SQLITE_INTEGER => {
                        ValueRef::Integer(ffi::sqlite3_column_int64(self.statement, *role))
                    }
                    ffi::SQLITE_FLOAT => {
                        ValueRef::Real(ffi::sqlite3_column_double(self.statement, *role))
                    }
                    ffi::SQLITE_TEXT => ValueRef::Text(std::slice::from_raw_parts(
                        ffi::sqlite3_column_text(self.statement, *role),
                        ffi::sqlite3_column_bytes(self.statement, *role) as usize,
                    )),
                    ffi::SQLITE_BLOB => {
                        let n = ffi::sqlite3_column_bytes(self.statement, *role) as usize;
                        ValueRef::Blob(if n == 0 {
                            &[]
                        } else {
                            std::slice::from_raw_parts(
                                ffi::sqlite3_column_blob(self.statement, *role).cast::<u8>(),
                                n,
                            )
                        })
                    }
                    _ => ValueRef::Null,
                }
            };
            match value {
                ValueRef::Integer(n) => ctx.set_result(&n),
                ValueRef::Real(n) => ctx.set_result(&n),
                ValueRef::Text(s) => {
                    ctx.set_result(&std::str::from_utf8(s).map_err(|e| error(e.to_string()))?)
                }
                ValueRef::Blob(b) => ctx.set_result(&b),
                ValueRef::Null => ctx.set_result(&rusqlite::types::Null),
            }
        } else {
            ctx.set_result(&rusqlite::types::Null)
        }
    }
    fn rowid(&self) -> Result<i64> {
        Ok(unsafe { ffi::sqlite3_column_int64(self.statement, 0) })
    }
}

unsafe extern "C" fn rename(raw: *mut ffi::sqlite3_vtab, new: *const c_char) -> c_int {
    // The adapter is the only custom callback with fallible work. Never unwind into C.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let table = unsafe { &mut *raw.cast::<Table>() };
        table.refresh()?;
        let name = unsafe { CStr::from_ptr(new) }
            .to_str()
            .map_err(|e| error(e.to_string()))?;
        table.rename_to(name)
    }))
    .unwrap_or_else(|_| Err(error("panic in virtual-table rename")));
    match result {
        Ok(()) => ffi::SQLITE_OK,
        Err(e) => unsafe { rusqlite::to_sqlite_error(&e, &mut (*raw).zErrMsg) },
    }
}
unsafe extern "C" fn shadow_name(name: *const c_char) -> c_int {
    let suffix = unsafe { CStr::from_ptr(name) }.to_bytes();
    i32::from(
        matches!(suffix, b"state" | b"delta" | b"keys")
            || (suffix.starts_with(b"op")
                && suffix[2..]
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric())),
    )
}
