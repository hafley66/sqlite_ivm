use crate::{
    catalog, maintenance,
    query::{bind, error, quote, Query},
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
        let mut raw: ffi::sqlite3_module = std::mem::transmute(Module::<Table>::update_module());
        raw.iVersion = 3;
        raw.xRename = Some(rename);
        raw.xShadowName = Some(shadow_name);
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
    roles: Vec<c_int>,
    generic: bool,
    query: Option<Query>,
    plan: Option<crate::relational::Plan>,
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
        self.db
            .prepare_cached("SELECT name FROM main.__ivm_views WHERE id=?1")?
            .query_row([self.id], |r| r.get(0))
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
        let stored: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_schema')",
            [],
            |r| r.get(0),
        )?;
        if stored {
            let versioned:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('__ivm_schema','main') WHERE name='format_version')",[],|r|r.get(0))?;
            if !versioned {
                return Err(error("sqlite_ivm storage format 1 requires the matching older extension; automatic migration is unavailable"));
            }
            let incompatible: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM main.__ivm_schema WHERE format_version!=2)",
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
            conn.query_row(
                "SELECT query_sql FROM main.__ivm_views WHERE name=?1",
                [name],
                |r| r.get(0),
            )?
        };
        if !create {
            let (id,declaration,generic,roles):(i64,String,bool,String)=conn.query_row("SELECT v.id,s.declaration,s.generic,s.roles FROM main.__ivm_views v JOIN main.__ivm_schema s ON s.id=v.id WHERE v.name=?1",[name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let roles = roles
                .split(',')
                .map(|s| {
                    s.parse::<c_int>()
                        .map_err(|_| error("invalid stored schema"))
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok((
                Cow::Owned(CString::new(declaration).map_err(|e| error(e.to_string()))?),
                Self {
                    base: ffi::sqlite3_vtab::default(),
                    db: conn,
                    id,
                    sql: String::new(),
                    roles,
                    generic,
                    query: None,
                    plan: None,
                },
            ));
        }
        let narrow=bind(&conn,&sql).and_then(|query|{
        for table in &query.tables {
            let effects:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1,'main')) OR EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='trigger' AND tbl_name=?1 AND substr(name,1,6)!='__ivm_')",[table],|r|r.get(0))?;
            if effects{return Err(error("cascades and source triggers require relational arrangements"));}
        }
        for c in maintenance::used_columns(&query) {
            let guaranteed:bool=conn.query_row("SELECT \"notnull\" OR (pk=1 AND upper(type)='INTEGER' AND NOT EXISTS(SELECT 1 FROM pragma_index_list(?1) WHERE origin='pk')) FROM pragma_table_info(?1) WHERE name=?2",rusqlite::params![query.tables[c.source],c.name],|r|r.get(0))?;
            if !guaranteed{return Err(error("nullable columns require relational maintenance"));}
        }
        Ok(query)
    });
        let (query, plan) = match narrow {
            Ok(query) => {
                maintenance::install(&conn, name, &sql)?;
                (Some(query), None)
            }
            Err(_) => {
                let plan = crate::relational::bind(&conn, &sql)?;
                crate::relational_maintenance::install(&conn, name, &sql, &plan)?;
                (None, Some(plan))
            }
        };
        let id = conn.query_row(
            "SELECT id FROM main.__ivm_views WHERE name=?1",
            [name],
            |r| r.get(0),
        )?;
        let declaration=format!("CREATE TABLE x({},__ivm_source INTEGER HIDDEN,__ivm_adding INTEGER HIDDEN,__ivm_row TEXT HIDDEN)",
        if let Some(query)=&query {query.outputs.iter().map(|(name,_)|format!("{} INTEGER",quote(name))).collect::<Vec<_>>().join(",")}else{let plan=plan.as_ref().unwrap();plan.names.iter().zip(&plan.nodes[plan.output].fields).map(|(name,f)|format!("{} {} COLLATE {}",quote(name),f.affinity,f.collation)).collect::<Vec<_>>().join(",")});
        let generic = plan.is_some();
        let roles: Vec<c_int> = if let Some(query) = &query {
            query
                .outputs
                .iter()
                .map(|(_, role)| match *role {
                    "g" => 0,
                    "n" => 1,
                    _ => 2,
                })
                .collect()
        } else {
            (1..=plan.as_ref().unwrap().names.len() as c_int).collect()
        };
        conn.execute_batch("CREATE TABLE IF NOT EXISTS main.__ivm_schema(id INTEGER PRIMARY KEY,declaration TEXT NOT NULL,generic INTEGER NOT NULL,roles TEXT NOT NULL,format_version INTEGER NOT NULL)")?;
        conn.execute(
            "INSERT INTO main.__ivm_schema VALUES(?1,?2,?3,?4,2)",
            rusqlite::params![
                id,
                declaration,
                generic,
                roles
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ],
        )?;
        Ok((
            Cow::Owned(CString::new(declaration).map_err(|e| error(e.to_string()))?),
            Self {
                base: ffi::sqlite3_vtab::default(),
                db: conn,
                id,
                sql,
                roles,
                generic,
                query,
                plan,
            },
        ))
    }
    fn refresh(&mut self) -> Result<()> {
        let sql: String = self
            .db
            .prepare_cached("SELECT query_sql FROM main.__ivm_views WHERE id=?1")?
            .query_row([self.id], |r| r.get(0))?;
        if sql != self.sql {
            if !self.generic {
                self.query = Some(bind(&self.db, &sql)?);
            } else {
                self.plan = Some(crate::relational::bind(&self.db, &sql)?);
            }
            self.sql = sql;
        }
        Ok(())
    }
    fn rename_to(&self, new: &str) -> Result<()> {
        maintenance::validate_name(new)?;
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
                self.db
                    .execute_batch(&format!(
                        "DROP {} main.{}",
                        kind.to_uppercase(),
                        quote(name)
                    ))
                    .map_err(|e| error(format!("rename dropping {kind} {name}: {e}")))?;
            }
        }
        for (kind, name, _) in &objects {
            if kind == "table" {
                let suffix = name
                    .strip_prefix(&format!("{old}_"))
                    .ok_or_else(|| error("invalid shadow table name"))?;
                let target = format!("{new}_{suffix}");
                self.db
                    .execute_batch(&format!(
                        "ALTER TABLE main.{} RENAME TO {}",
                        quote(name),
                        quote(&target)
                    ))
                    .map_err(|e| error(format!("rename shadow {name}: {e}")))?;
                renamed.push(("table", target));
            }
        }
        renamed.extend(if let Some(query) = &self.query {
            maintenance::create_hooks(&self.db, new, query, false)?
        } else {
            crate::relational_maintenance::hooks(&self.db, new, self.plan.as_ref().unwrap())?
        });
        self.db
            .execute("DELETE FROM main.__ivm_objects WHERE view_name=?1", [&old])?;
        for table in ["__ivm_sources", "__ivm_columns"] {
            self.db.execute(
                &format!("UPDATE main.{table} SET view_name=?1 WHERE view_name=?2"),
                [new, &old],
            )?;
        }
        self.db.execute(
            "UPDATE main.__ivm_views SET name=?1 WHERE id=?2",
            rusqlite::params![new, self.id],
        )?;
        for (kind, name) in renamed {
            let ddl: String = self.db.query_row(
                "SELECT sql FROM main.sqlite_schema WHERE type=?1 AND name=?2",
                [kind, &name],
                |r| r.get(0),
            )?;
            self.db.execute(
                "INSERT INTO main.__ivm_objects VALUES(?1,?2,?3,?4)",
                [new, kind, &name, &ddl],
            )?;
        }
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
        Self::attach(db, schema, name, args, false)
    }
    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        if self.generic {
            info.set_estimated_cost(1_000_000.0);
            return Ok(true);
        }
        let group = self.roles.iter().position(|role| *role == 0).unwrap() as c_int;
        let mut equality = false;
        for (constraint, mut usage) in info.constraints_and_usages() {
            if !equality
                && constraint.is_usable()
                && (constraint.column() == group || constraint.column() == -1)
                && constraint.operator() == IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
            {
                usage.set_argv_index(1);
                // Let SQLite recheck affinity/collation semantics on the exposed column.
                equality = true;
            }
        }
        info.set_idx_num(i32::from(equality));
        info.set_idx_str(if equality { "group_key" } else { "stored_scan" });
        info.set_estimated_cost(if equality { 1.0 } else { 1_000_000.0 });
        info.set_estimated_rows(if equality { 1 } else { 1_000_000 });
        Ok(true)
    }
    fn open(&'vtab mut self) -> Result<Cursor> {
        Ok(Cursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            db: unsafe { self.db.handle() },
            state: format!("{}_state", self.name()?),
            roles: self.roles.clone(),
            generic: self.generic,
            statement: ptr::null_mut(),
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
        if args.len() != count + 5 || args.iter().take(count + 2).any(|v| v != ValueRef::Null) {
            return Err(error("managed results are read-only"));
        }
        let source = usize::try_from(args.get::<i64>(count + 2)?)
            .map_err(|_| error("invalid source ordinal"))?;
        let adding: i64 = args.get(count + 3)?;
        let payload: String = args.get(count + 4)?;
        if !(0..=1).contains(&adding) {
            return Err(error("invalid maintenance command"));
        }
        if let Some(plan) = &self.plan {
            let src = plan
                .sources
                .get(source)
                .ok_or_else(|| error("invalid source ordinal"))?;
            let row=self.db.prepare_cached("SELECT CASE WHEN type='array' THEN CASE json_extract(value,'$[0]') WHEN 'blob' THEN unhex(json_extract(value,'$[1]')) WHEN 'real' THEN CAST(json_extract(value,'$[1]') AS REAL) ELSE json_extract(value,'$[1]') END ELSE value END FROM json_each(?1) ORDER BY key")?.query_map([&payload],|r|r.get::<_,rusqlite::types::Value>(0))?.collect::<Result<Vec<_>>>()?;
            if row.len() != src.columns.len() {
                return Err(error("invalid maintenance row"));
            }
            plan.input(
                &self.db,
                &self.name()?,
                source,
                row,
                if adding == 1 { 1 } else { -1 },
            )?;
            return Ok(0);
        }
        let query = self.query.as_ref().unwrap();
        if source >= query.tables.len() {
            return Err(error("invalid source ordinal"));
        }
        let used = maintenance::used_columns(query);
        let columns = used
            .into_iter()
            .filter(|c| c.source == source)
            .collect::<Vec<_>>();
        let valid: bool = self.db.query_row(
            "SELECT json_valid(?1) AND json_type(?1)='array' AND json_array_length(?1)=?2",
            rusqlite::params![payload, columns.len() as i64],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(error("invalid maintenance row"));
        }
        let integers: bool = self.db.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM json_each(?1) WHERE type!='integer')",
            [&payload],
            |r| r.get(0),
        )?;
        if !integers {
            return Err(error("maintenance rows require integers"));
        }
        maintenance::maintain(
            &self.db,
            &self.name()?,
            query,
            source,
            adding == 1,
            &payload,
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
    generic: bool,
    statement: *mut ffi::sqlite3_stmt,
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
    fn filter(&mut self, plan: c_int, _: Option<&str>, args: &Filters<'_>) -> Result<()> {
        unsafe {
            ffi::sqlite3_finalize(self.statement);
        }
        self.statement = ptr::null_mut();
        self.done = true;
        let sql = CString::new(format!(
            "SELECT {} FROM main.{}{}",
            if self.generic {
                format!(
                    "rowid,{}",
                    (0..self.roles.len())
                        .map(|i| format!("c{i}"))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else {
                "g,n,s".into()
            },
            quote(&self.state),
            if plan == 1 { " WHERE g=?1" } else { "" }
        ))
        .map_err(|e| error(e.to_string()))?;
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
        if plan == 1 {
            let value = args
                .iter()
                .next()
                .ok_or_else(|| error("missing group key"))?;
            let rc = unsafe {
                match value {
                    ValueRef::Null => ffi::sqlite3_bind_null(self.statement, 1),
                    ValueRef::Integer(n) => ffi::sqlite3_bind_int64(self.statement, 1, n),
                    ValueRef::Real(n) => ffi::sqlite3_bind_double(self.statement, 1, n),
                    ValueRef::Text(s) => ffi::sqlite3_bind_text(
                        self.statement,
                        1,
                        s.as_ptr().cast(),
                        s.len() as c_int,
                        ffi::SQLITE_TRANSIENT(),
                    ),
                    ValueRef::Blob(s) => ffi::sqlite3_bind_blob(
                        self.statement,
                        1,
                        s.as_ptr().cast(),
                        s.len() as c_int,
                        ffi::SQLITE_TRANSIENT(),
                    ),
                }
            };
            self.check(rc)?;
        }
        self.next()
    }
    fn next(&mut self) -> Result<()> {
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
        matches!(suffix, b"state" | b"delta")
            || (suffix.starts_with(b"op")
                && suffix[2..].iter().all(|c| c.is_ascii_digit() || *c == b'_')),
    )
}
