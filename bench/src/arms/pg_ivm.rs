//! pg_ivm arm: temp cluster via initdb/pg_ctl, `CREATE EXTENSION pg_ivm`,
//! `pgivm.create_immv`, per-state loop; SQLSTATE 0A000 => `Setup::Unsupported`.

use super::{verify_inputs, verify_output, Arm, Measure, Setup};
use crate::fixture::{Fixture, State, WriteRow};
use crate::oracle::Cell;
use anyhow::{anyhow, bail, Result};
use postgres::error::SqlState;
use postgres::{Client, NoTls, Row};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use sysinfo::{Pid, ProcessesToUpdate, System};

/// One PostgreSQL cluster per shootout invocation; `pg_ctl -m immediate stop`
/// on drop.
pub struct Cluster {
    prefix: PathBuf,
    dir: PathBuf,
    pub socket: PathBuf,
    user: String,
    pub dbname: &'static str,
}

impl Cluster {
    pub fn start(prefix: &Path, log_dir: &Path) -> Result<Arc<Cluster>> {
        let dir = std::env::temp_dir().join(format!("ivm-bench-pg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let socket = dir.join("socket");
        let data = dir.join("data");
        std::fs::create_dir_all(&socket)?;
        let cluster = Cluster {
            prefix: prefix.to_path_buf(),
            dir: dir.clone(),
            socket,
            user: whoami(),
            dbname: "pgivm_bench",
        };
        super::run_logged(
            &log_dir.join("postgres-init.log"),
            &prefix.join("bin/initdb"),
            &[
                data.as_os_str(),
                std::ffi::OsStr::new("--auth=trust"),
                std::ffi::OsStr::new("--no-locale"),
                std::ffi::OsStr::new("--encoding=UTF8"),
            ],
        )?;
        let settings = format!(
            "-c listen_addresses='' -c unix_socket_directories='{}' \
             -c shared_preload_libraries='pg_ivm' -c shared_buffers=32MB -c work_mem=1MB \
             -c temp_file_limit=128MB -c statement_timeout=120000 -c max_connections=16 \
             -c fsync=on -c synchronous_commit=on -c full_page_writes=on",
            cluster.socket.display()
        );
        super::run_logged(
            &log_dir.join("postgres-start.log"),
            &prefix.join("bin/pg_ctl"),
            &[
                std::ffi::OsStr::new("-D"),
                data.as_os_str(),
                std::ffi::OsStr::new("-l"),
                log_dir.join("postgres-server.log").as_os_str(),
                std::ffi::OsStr::new("-o"),
                std::ffi::OsStr::new(&settings),
                std::ffi::OsStr::new("start"),
                std::ffi::OsStr::new("-w"),
            ],
        )?;
        let mut bootstrap = cluster.client("postgres")?;
        bootstrap.batch_execute(&format!("CREATE DATABASE {}", cluster.dbname))?;
        drop(bootstrap);
        Ok(Arc::new(cluster))
    }

    pub fn client(&self, dbname: &str) -> Result<Client> {
        let params = format!(
            "host={} port=5432 user={} dbname={}",
            self.socket.display(),
            self.user,
            dbname
        );
        Ok(Client::connect(&params, NoTls)?)
    }

    /// Postmaster pid from `data/postmaster.pid`, first line.
    pub fn postmaster_pid(&self) -> Result<Pid> {
        let text = std::fs::read_to_string(self.dir.join("data/postmaster.pid"))?;
        let first = text.lines().next().ok_or_else(|| anyhow!("empty postmaster.pid"))?;
        Ok(Pid::from_u32(first.trim().parse()?))
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = std::process::Command::new(self.prefix.join("bin/pg_ctl"))
            .args([
                "-D",
                &self.dir.join("data").to_string_lossy(),
                "-m",
                "immediate",
                "stop",
            ])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string())
}

/// Summed RSS over the postmaster descendant group, sampled after each state
/// (outside the timed windows); peak retained per case.
pub struct PgGroup {
    sys: System,
    pids: Vec<Pid>,
    peak: u64,
}

impl PgGroup {
    pub fn new(postmaster: Pid) -> Result<PgGroup> {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut pids = vec![postmaster];
        loop {
            let mut added = Vec::new();
            for (pid, process) in sys.processes() {
                let is_child = process
                    .parent()
                    .map(|parent| pids.contains(&parent) && !pids.contains(pid))
                    .unwrap_or(false);
                if is_child {
                    added.push(*pid);
                }
            }
            if added.is_empty() {
                break;
            }
            pids.extend(added);
        }
        Ok(PgGroup { sys, pids, peak: 0 })
    }

    pub fn sample(&mut self) -> u64 {
        self.sys
            .refresh_processes(ProcessesToUpdate::Some(&self.pids), false);
        let total: u64 = self
            .pids
            .iter()
            .filter_map(|pid| self.sys.process(*pid))
            .map(|process| process.memory())
            .sum();
        self.peak = self.peak.max(total);
        total
    }
}

pub(crate) fn create_tables(db: &mut Client) -> Result<()> {
    db.batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")?;
    for table in ["a", "b", "c"] {
        db.batch_execute(&format!(
            "CREATE TABLE {table}(id bigint PRIMARY KEY,k bigint NOT NULL,v bigint NOT NULL);\
             CREATE INDEX ON {table}(k);CREATE INDEX ON {table}(v)"
        ))?;
    }
    Ok(())
}
fn c_list(columns: usize) -> String {
    (0..columns).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",")
}

/// All shootout outputs are integral; numeric/float columns are cast through
/// float8 and re-checked as integers on read.
pub(crate) fn pg_projection(columns: usize, inner: &str) -> String {
    let cells: Vec<String> =
        (0..columns).map(|i| format!("c{i}::float8 AS c{i}")).collect();
    format!("SELECT {} FROM ({inner}) AS q", cells.join(","))
}

pub(crate) fn pg_rows(rows: Vec<Row>) -> Result<Vec<Vec<Cell>>> {
    rows
        .into_iter()
        .map(|row| {
            (0..row.len())
                .map(|i| {
                    let column = row.columns()[i].name().to_string();
                    let kind = row.columns()[i].type_().name().to_string();
                    let value: f64 = row.try_get(i).map_err(|error| {
                        anyhow!("column {i} ({column}, pg type {kind}): {error}")
                    })?;
                    if value.fract() != 0.0 || value.abs() >= 9.0e15 {
                        bail!("column {i} is not an integral cell: {value}");
                    }
                    Ok(Cell::Int(value as i64))
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect()
}
pub(crate) fn pg_write_rows(client: &mut Client, table: &str) -> Result<Vec<WriteRow>> {
    let rows = client.query(&format!("SELECT id,k,v FROM {table}"), &[])?;
    rows
        .into_iter()
        .map(|row| Ok(WriteRow(row.get(0), Cell::Int(row.get(1)), Cell::Int(row.get(2)))))
        .collect()
}

pub struct PgIvm {
    cluster: Arc<Cluster>,
    query: String,
    materialize: String,
    columns: usize,
    db: Option<Client>,
    group: Option<PgGroup>,
    disk: Option<u64>,
}

impl PgIvm {
    pub fn new(cluster: Arc<Cluster>) -> Self {
        Self {
            cluster,
            query: String::new(),
            materialize: String::new(),
            columns: 0,
            db: None,
            group: None,
            disk: None,
        }
    }
}

impl Arm for PgIvm {
    fn name(&self) -> &'static str {
        "pg-ivm"
    }

    fn setup(&mut self, fixture: &Fixture) -> Result<Setup> {
        let mut db = self.cluster.client(self.cluster.dbname)?;
        db.batch_execute("DROP EXTENSION IF EXISTS pg_ivm CASCADE")?;
        create_tables(&mut db)?;
        db.batch_execute("CREATE EXTENSION pg_ivm")?;
        self.columns = fixture.column_count();
        self.query = fixture.query.clone();
        self.materialize = pg_projection(
            self.columns,
            &format!("SELECT {} FROM circuit_view", c_list(self.columns)),
        );
        let mut tx = db.transaction()?;
        match tx.query("SELECT pgivm.create_immv($1,$2)", &[&"circuit_view", &self.query]) {
            Err(error)
                if error
                    .as_db_error()
                    .is_some_and(|db_error| db_error.code() == &SqlState::FEATURE_NOT_SUPPORTED) =>
            {
                let db_error = error.as_db_error().expect("matched SQLSTATE above");
                return Ok(Setup::Unsupported {
                    reason: format!("{} (SQLSTATE {})", db_error.message(), db_error.code().code()),
                });
            }
            other => {
                other?;
                tx.commit()?;
            }
        }
        let mut group = PgGroup::new(self.cluster.postmaster_pid()?)?;
        group.sample();
        self.group = Some(group);
        self.db = Some(db);
        Ok(Setup::Ready)
    }

    fn apply(&mut self, state: &State) -> Result<Measure> {
        let db = self.db.as_mut().expect("setup before apply");
        let started = Instant::now();
        db.batch_execute(&format!("BEGIN;{} COMMIT;", state.mutation_sql))?;
        let actual = pg_rows(db.query(&self.materialize, &[])?)?;
        let wall = started.elapsed();
        self.group.as_mut().expect("group sampler").sample();
        let bytes: i64 = db
            .query_one("SELECT pg_database_size(current_database())::bigint AS bytes", &[])?
            .get(0);
        self.disk = Some(bytes as u64);
        let oracle = pg_rows(db.query(&pg_projection(self.columns, &self.query), &[])?)?;
        let checksum = verify_output(state, actual, Some(oracle))?;
        verify_inputs(state, |table| pg_write_rows(db, table))?;
        Ok(Measure { wall, checksum })
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(db) = self.db.as_mut() {
            self.disk = Some(
                db.query_one("SELECT pg_database_size(current_database()) AS bytes", &[])?
                    .get::<_, i64>(0) as u64,
            );
            self.group.as_mut().expect("group sampler").sample();
        }
        self.db = None;
        Ok(())
    }

    fn peak_rss_bytes(&self) -> Option<u64> {
        self.group.as_ref().map(|group| group.peak).filter(|peak| *peak > 0)
    }

    fn disk_bytes(&self) -> Option<u64> {
        self.disk
    }
}
