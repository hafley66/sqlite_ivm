//! Session-extension probes. Each gate question gets the smallest mechanism
//! that can answer it, and a gate answering "no" is a valid result, so probes
//! return findings instead of asserting outcomes.

use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator as _;
use rusqlite::hooks::Action;
use rusqlite::session::{Changeset, Session};
use rusqlite::types::ValueRef;
use rusqlite::vtab::{
    Context, CreateVTab, Filters, IndexInfo, Inserts, Module, UpdateVTab, Updates, VTab,
    VTabConnection, VTabCursor, VTabKind,
};
use rusqlite::{Connection, Result};
use std::borrow::Cow;
use std::ffi::CStr;
use std::os::raw::c_int;

/// Beyond this many entries a probe is no longer measuring the mechanism it
/// named; stop with a diagnostic instead of draining an unbounded changeset.
const MAX_CHANGESET_ENTRIES: usize = 1000;

/// The vtab probe writes a handful of rows; the cap keeps a runaway xUpdate
/// from turning a bounded probe into an unbounded write loop.
const MAX_SCRIBED_ROWS: i64 = 100;

pub struct Finding {
    pub yes: bool,
    pub detail: String,
}

pub struct Entry {
    pub table: String,
    pub action: Action,
    pub indirect: bool,
}

fn error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(message.into()),
    )
}

/// Every changeset a probe reads goes through here, so the budget holds
/// everywhere and entries arrive as plain data for tests and main to print.
pub fn drain(changeset: &Changeset) -> Result<Vec<Entry>> {
    let mut iter = changeset.iter()?;
    let mut entries = Vec::new();
    while let Some(item) = iter.next()? {
        if entries.len() >= MAX_CHANGESET_ENTRIES {
            return Err(error(format!(
                "changeset exceeded the {MAX_CHANGESET_ENTRIES} entry probe budget"
            )));
        }
        let op = item.op()?;
        entries.push(Entry {
            table: op.table_name().to_owned(),
            action: op.code(),
            indirect: op.indirect(),
        });
    }
    Ok(entries)
}

/// Gate 1, runtime half: the lab only builds with the session feature, and a
/// live session proves the C-level API exists on this toolchain.
pub fn probe_session_api(db: &Connection) -> Result<Finding> {
    let mut session = Session::new(db)?;
    session.attach::<&str>(None)?;
    let enabled = session.is_enabled();
    Ok(Finding {
        yes: enabled,
        detail: format!("session attached, enabled={enabled}"),
    })
}

/// Gate 2: a table with no declared PRIMARY KEY, one insert, one changeset.
pub fn probe_no_primary_key(db: &Connection) -> Result<Finding> {
    db.execute_batch("CREATE TABLE nopk(note TEXT NOT NULL)")?;
    let mut session = Session::new(db)?;
    session.attach::<&str>(None)?;
    db.execute("INSERT INTO nopk VALUES ('seen')", [])?;
    let entries = drain(&session.changeset()?)?;
    let hits = entries.iter().filter(|e| e.table == "nopk").count();
    Ok(Finding {
        yes: hits > 0,
        detail: format!("{hits} entries recorded for the PRIMARY KEY-less table"),
    })
}

/// Gate 3: the session is attached to the real table, and the row arrives
/// through the vtab's xUpdate, mirroring this repo's maintenance path.
pub fn probe_vtab_writes(db: &Connection) -> Result<Finding> {
    const SCRIBE: Module<ScribeTable> = Module::update_module();
    db.execute_batch(
        "CREATE TABLE scribe_log(id INTEGER PRIMARY KEY, note TEXT NOT NULL)",
    )?;
    db.create_module("scribe", &SCRIBE, None)?;
    db.execute_batch("CREATE VIRTUAL TABLE scribe_v USING scribe()")?;
    let mut session = Session::new(db)?;
    session.attach::<&str>(None)?;
    db.execute("INSERT INTO scribe_v VALUES ('through xUpdate')", [])?;
    let entries = drain(&session.changeset()?)?;
    let landed: i64 = db.query_row("SELECT COUNT(*) FROM scribe_log", [], |r| r.get(0))?;
    let logged = entries
        .iter()
        .filter(|e| e.table == "scribe_log")
        .count();
    let direct = entries.iter().filter(|e| e.table == "scribe_v").count();
    Ok(Finding {
        yes: logged == 1 && landed == 1,
        detail: format!(
            "{landed} rows landed in the real table, {logged} changeset entries from xUpdate, \
             {direct} for the vtab write itself"
        ),
    })
}

/// Gate 4a: insert, update, and an insert-delete pair; the claimed advantage
/// over per-row hooks is one entry left standing.
pub fn probe_coalesce(db: &Connection) -> Result<Finding> {
    db.execute_batch("CREATE TABLE t(k INTEGER PRIMARY KEY, v INTEGER)")?;
    let mut session = Session::new(db)?;
    session.attach::<&str>(None)?;
    db.execute_batch(
        "INSERT INTO t VALUES (1, 1);
         UPDATE t SET v = 2 WHERE k = 1;
         INSERT INTO t VALUES (2, 9);
         DELETE FROM t WHERE k = 2;",
    )?;
    let entries = drain(&session.changeset()?)?;
    let hits = entries.iter().filter(|e| e.table == "t").count();
    Ok(Finding {
        yes: hits == 1,
        detail: format!("{hits} entries for the three statements"),
    })
}

/// Gate 4b: one update, then changeset and patchset compared on old(v).
pub fn probe_old_values(db: &Connection) -> Result<Finding> {
    db.execute_batch(
        "CREATE TABLE t(k INTEGER PRIMARY KEY, v INTEGER);
         INSERT INTO t VALUES (1, 1);",
    )?;
    let mut session = Session::new(db)?;
    session.attach::<&str>(None)?;
    db.execute("UPDATE t SET v = 2 WHERE k = 1", [])?;
    let changeset = session.changeset()?;
    let patchset = session.patchset()?;
    let changeset_old = {
        let mut iter = changeset.iter()?;
        let item = iter
            .next()?
            .ok_or_else(|| error("changeset is missing the update entry"))?;
        format!("{:?}", item.old_value(1))
    };
    let patchset_old = {
        let mut iter = patchset.iter()?;
        let item = iter
            .next()?
            .ok_or_else(|| error("patchset is missing the update entry"))?;
        format!("{:?}", item.old_value(1))
    };
    let keeps = changeset_old.contains("Integer(1)");
    let strips = patchset_old.contains("unavailable")
        || patchset_old.contains("Invalid")
        || patchset_old.contains("Err");
    Ok(Finding {
        yes: keeps && strips,
        detail: format!("changeset old(v)={changeset_old}; patchset old(v)={patchset_old}"),
    })
}

#[repr(C)]
struct ScribeTable {
    base: rusqlite::ffi::sqlite3_vtab,
    db: Connection, // Non-owning handle, valid for this virtual-table connection.
}

impl ScribeTable {
    fn scribe(&mut self, note: &str) -> Result<i64> {
        let logged: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM scribe_log", [], |r| r.get(0))?;
        if logged >= MAX_SCRIBED_ROWS {
            return Err(error(format!(
                "scribe_log hit the {MAX_SCRIBED_ROWS} row probe budget"
            )));
        }
        self.db
            .execute("INSERT INTO scribe_log(note) VALUES (?1)", [note])?;
        Ok(self.db.last_insert_rowid())
    }
}

unsafe impl<'vtab> VTab<'vtab> for ScribeTable {
    type Aux = ();
    type Cursor = ScribeCursor;

    fn connect(
        db: &mut VTabConnection,
        _: Option<&()>,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let conn = unsafe { Connection::from_handle(db.handle())? };
        Ok((
            Cow::Borrowed(c"CREATE TABLE x(v TEXT)"),
            Self {
                base: rusqlite::ffi::sqlite3_vtab::default(),
                db: conn,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        info.set_estimated_cost(1.0);
        Ok(true)
    }

    fn open(&'vtab mut self) -> Result<ScribeCursor> {
        Ok(ScribeCursor {
            base: rusqlite::ffi::sqlite3_vtab_cursor::default(),
        })
    }
}

impl<'vtab> CreateVTab<'vtab> for ScribeTable {
    const KIND: VTabKind = VTabKind::Default;

    fn create(
        db: &mut VTabConnection,
        _: Option<&()>,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        VTab::connect(db, None, &[], &[], &[], &[])
    }
}

impl<'vtab> UpdateVTab<'vtab> for ScribeTable {
    fn delete(&mut self, _: ValueRef<'_>) -> Result<()> {
        Err(error("probe vtab is insert-only"))
    }

    fn update(&mut self, _: &Updates<'_>) -> Result<()> {
        Err(error("probe vtab is insert-only"))
    }

    fn insert(&mut self, args: &Inserts<'_>) -> Result<i64> {
        let note: String = args.get(2)?;
        self.scribe(&note)
    }
}

/// Reads are never exercised: the probe only inserts through the vtab, so the
/// cursor is born at end of scan.
#[repr(C)]
struct ScribeCursor {
    base: rusqlite::ffi::sqlite3_vtab_cursor,
}

unsafe impl VTabCursor for ScribeCursor {
    fn filter(&mut self, _: c_int, _: Option<&str>, _: &Filters<'_>) -> Result<()> {
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        Ok(())
    }

    fn eof(&self) -> bool {
        true
    }

    fn column(&self, _: &mut Context, _: c_int) -> Result<()> {
        Ok(())
    }

    fn rowid(&self) -> Result<i64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().expect("in-memory database opens")
    }

    #[test]
    fn session_feature_compiles() {
        let finding = probe_session_api(&db()).expect("session api probe runs");
        eprintln!("gate 1 runtime: yes={}, {}", finding.yes, finding.detail);
    }

    #[test]
    fn session_records_a_table_without_a_declared_primary_key() {
        let finding = probe_no_primary_key(&db()).expect("no-PK probe runs");
        eprintln!("gate 2: yes={}, {}", finding.yes, finding.detail);
    }

    #[test]
    fn session_sees_writes_that_went_through_xupdate() {
        let finding = probe_vtab_writes(&db()).expect("vtab probe runs");
        eprintln!("gate 3: yes={}, {}", finding.yes, finding.detail);
    }

    #[test]
    fn changeset_coalesces_three_statements_into_one_entry() {
        let finding = probe_coalesce(&db()).expect("coalesce probe runs");
        eprintln!("gate 4a: yes={}, {}", finding.yes, finding.detail);
    }

    #[test]
    fn changeset_carries_old_values_and_patchset_does_not() {
        let finding = probe_old_values(&db()).expect("old-values probe runs");
        eprintln!("gate 4b: yes={}, {}", finding.yes, finding.detail);
    }
}
