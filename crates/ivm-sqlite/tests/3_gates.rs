//! SQLite VM work after a load, using the same K1 sizes as the lab gate.

#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_dd::{Engine, Frontier, Program, Raw, SourceChange};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};

extern "C" fn tick(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe { &*(counter as *const AtomicU64) }.fetch_add(1, Ordering::Relaxed);
    0
}

fn steps_of_one_change(program: &Program, load: Vec<SourceChange>, change: SourceChange) -> u64 {
    let db = Connection::open_in_memory().unwrap();
    let mut sql = Sqlite::install(program, &mut Raw::with_connection(&db)).unwrap();
    sql.settle(Frontier { changes: load }, &mut Raw::with_connection(&db)).unwrap();
    let steps = Arc::new(AtomicU64::new(0));
    let counter = Arc::as_ptr(&steps) as *mut std::ffi::c_void;
    unsafe { rusqlite::ffi::sqlite3_progress_handler(db.handle(), 1, Some(tick), counter); }
    sql.settle(Frontier { changes: vec![change] }, &mut Raw::with_connection(&db)).unwrap();
    unsafe { rusqlite::ffi::sqlite3_progress_handler(db.handle(), 0, None, std::ptr::null_mut()); }
    steps.load(Ordering::Relaxed)
}

#[test]
fn k1_sqlite_one_row_change_work_is_independent_of_loaded_size() {
    let row = |rel, row: Vec<i64>| SourceChange { rel, row, w: 1 };
    let access = support::program("0_access");
    let access_at = |n: i64| {
        let mut load = vec![row(1, vec![10, 100])];
        load.extend((0..n).map(|person| row(0, vec![person, 10])));
        steps_of_one_change(&access, load, row(2, vec![-1, 7]))
    };
    let (small, large) = (access_at(1_000), access_at(30_000));
    println!("access: {small} VM steps at 1e3, {large} at 3e4");
    assert!(large <= small * 2, "access: {small} VM steps at 1e3, {large} at 3e4");

    let team_cost = support::program("1_team_cost");
    let team_cost_at = |n: i64| {
        let mut load: Vec<SourceChange> = (0..n).map(|id| row(0, vec![id, 1000 + id % 100, id])).collect();
        load.extend((0..5).map(|k| row(0, vec![-1 - k, 10, k])));
        steps_of_one_change(&team_cost, load, row(0, vec![-100, 10, 7]))
    };
    let (small, large) = (team_cost_at(1_000), team_cost_at(30_000));
    println!("team_cost: {small} VM steps at 1e3, {large} at 3e4");
    assert!(large <= small * 2, "team_cost: {small} VM steps at 1e3, {large} at 3e4");
}
