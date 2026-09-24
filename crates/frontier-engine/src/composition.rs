//! Composed programs: one producer's frontier view consumed by a second
//! frontier program, both settled by ordinary SQL on the base tables.
//!
//! One `sqlite_ext::watch` serves the whole composition. It takes the
//! producer's collector slot `frontier_{producer}_c{install}` and watches the
//! producer's base sources plus the consumer's own table sources. At COMMIT a
//! single `xSync` fires; its callback settles the producer first, maps the
//! producer's net output delta 1:1 onto the consumer's view relation, and
//! settles the consumer with its directly-watched rows appended. Both settles
//! run in Rust sequence inside that one callback: exactly one virtual table
//! exists in the transaction, written only by user SQL during statement
//! execution — never inside `xSync` — so there is no inter-callback order to
//! prove and install order is irrelevant.
//!
//! Why not one collector per program: the consumer's collector would watch
//! the producer's engine tables, whose writes fire inside the producer's own
//! `xSync`. SQLite rejects a virtual-table write while any vtab is mid-sync
//! (`sqlite3VtabBegin` returns `SQLITE_LOCKED` when `sqlite3VtabInSync`
//! holds, `vdbeaux.c`/`vtab.c`), so such a commit always fails.
//! `vtab_write_inside_xsync_is_rejected` in `tests/composition.rs`
//! reproduces this on the live connection.
//!
//! The consumer may join the producer's view with real tables. The join's
//! live-side reads of the view happen after the producer settle in the same
//! callback, so they see post-frontier truth and the three-term join
//! equation `W = dL ⋈ R + L ⋈ dR − dL ⋈ dR` stays valid for the derived
//! input. A consumer may read at most one program view (this producer's); a
//! producer may read none at all; both restrictions fail as explicit
//! `Unsupported`, never as a silent mis-settle.

use crate::catalog::{self, Installed};
use crate::engine;
use crate::error::{EngineError, ErrorKind, Stage};
use crate::{Frontier, Program, Sign, SourceChange};
use sqlite_ext::rusqlite::{self, Connection};
use sqlite_ext::{BulkTrigger, RowChange};
use std::sync::Arc;

/// A producer-consumer pair settling as one graph at COMMIT.
///
/// All state lives in the connection's schema and catalog; the handle owns no
/// connection. [`Composition::open`] re-registers the shared collector on the
/// same connection after the previous handles were dropped.
pub struct Composition {
    producer: Program,
    consumer: Program,
}

impl Composition {
    /// Installs the producer and consumer unwatched, validates the
    /// composition shape, and registers the one shared commit collector over
    /// `producer sources ∪ consumer table sources`. Any failure tears back
    /// everything this call installed.
    pub fn install(
        conn: &Connection,
        producer: (&str, &str),
        consumer: (&str, &str),
    ) -> Result<Self, EngineError> {
        let producer = Program::install_unwatched(conn, producer.0, producer.1)?;
        if let Err(e) = reject_derived_producer_sources(conn, &producer) {
            let _ = producer.teardown(conn);
            return Err(e);
        }
        let consumer = match Program::install_unwatched(conn, consumer.0, consumer.1) {
            Ok(consumer) => consumer,
            Err(e) => {
                let _ = producer.teardown(conn);
                return Err(e);
            }
        };
        if let Err(e) = validate_pair(conn, &producer, &consumer)
            .and_then(|()| register(conn, producer.handle(), consumer.handle()))
        {
            let _ = consumer.teardown(conn);
            let _ = producer.teardown(conn);
            return Err(e);
        }
        Ok(Self { producer, consumer })
    }

    /// Reopens both programs from the connection's catalog and re-registers
    /// the shared collector. Any collector instance left by earlier handles
    /// on this connection is dropped first; its schema objects (vtab,
    /// triggers) persist, so `watch` would collide with the live slot
    /// otherwise.
    pub fn open(conn: &Connection, producer: &str, consumer: &str) -> Result<Self, EngineError> {
        let producer = Program::open(conn, producer)?;
        let consumer = Program::open(conn, consumer)?;
        reject_derived_producer_sources(conn, &producer)?;
        validate_pair(conn, &producer, &consumer)?;
        drop_collector(conn, &producer)?;
        register(conn, producer.handle(), consumer.handle())?;
        Ok(Self { producer, consumer })
    }

    /// The producer program.
    pub fn producer(&self) -> &Program {
        &self.producer
    }

    /// The consumer program.
    pub fn consumer(&self) -> &Program {
        &self.consumer
    }

    /// Drops the shared collector vtab (its `xDestroy` removes every trigger
    /// it created, including the consumer's extra tables), then the consumer,
    /// then the producer.
    pub fn teardown(self, conn: &Connection) -> Result<(), EngineError> {
        drop_collector(conn, &self.producer)?;
        self.consumer.teardown(conn)?;
        self.producer.teardown(conn)
    }
}

fn reject_derived_producer_sources(conn: &Connection, producer: &Program) -> Result<(), EngineError> {
    let derived = catalog::scan_derived_sources(conn, &producer.handle())?;
    match derived.first() {
        Some(view) => Err(EngineError::unsupported(
            Stage::Install,
            view,
            "the producer of a composition must read base tables only",
        )),
        None => Ok(()),
    }
}

fn validate_pair(
    conn: &Connection,
    producer: &Program,
    consumer: &Program,
) -> Result<(), EngineError> {
    let view = producer.view();
    let mut seen = false;
    for source in consumer.sources() {
        if *source == view {
            if seen {
                return Err(EngineError::unsupported(
                    Stage::Install,
                    source,
                    "the consumer reads the producer's view more than once",
                ));
            }
            seen = true;
        } else if catalog::derived_view_program(conn, source)?.is_some() {
            return Err(EngineError::unsupported(
                Stage::Install,
                source,
                "a consumer may settle from at most one producer; install one Composition per program view",
            ));
        } else if !catalog::is_base_table(conn, source)? {
            return Err(EngineError::unsupported(
                Stage::Install,
                source,
                "only base tables can be watched for the composition collector",
            ));
        }
    }
    if !seen {
        return Err(EngineError::unsupported(
            Stage::Install,
            &view,
            "the consumer must read the producer's output view",
        ));
    }
    Ok(())
}

fn collector_name(producer: &Installed) -> String {
    catalog::collector(&producer.name, producer.install)
}

fn drop_collector(conn: &Connection, producer: &Program) -> Result<(), EngineError> {
    let name = collector_name(&producer.handle());
    conn.execute_batch(&format!("DROP TABLE IF EXISTS main.{};", catalog::quote(&name)))
        .map_err(|e| {
            EngineError::new(Stage::Install, &name, ErrorKind::Sqlite(e.to_string()))
        })
}

/// Registers the one shared collector: the producer's slot, watching the
/// producer's sources plus the consumer's non-view sources. The collector
/// name matches `catalog::collector(producer)`, so the producer's own
/// teardown also finds the slot free of leftovers.
fn register(
    conn: &Connection,
    producer: Arc<Installed>,
    consumer: Arc<Installed>,
) -> Result<(), EngineError> {
    let name = collector_name(&producer);
    let view = catalog::view(&producer.name);
    let mut watched = producer.sources.clone();
    for source in &consumer.sources {
        if *source != view && !watched.contains(source) {
            watched.push(source.clone());
        }
    }
    let tables: Vec<&str> = watched.iter().map(String::as_str).collect();
    sqlite_ext::watch(conn, &name, &tables, GroupTrigger { producer, consumer, view })
        .map_err(|e| EngineError::new(Stage::Install, &name, ErrorKind::Sqlite(e.to_string())))
}

/// The graph-owned commit callback. One instance per composition.
pub(crate) struct GroupTrigger {
    producer: Arc<Installed>,
    consumer: Arc<Installed>,
    view: String,
}

impl BulkTrigger for GroupTrigger {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> rusqlite::Result<()> {
        let mut producer_rows: Vec<SourceChange> = Vec::new();
        let mut consumer_rows: Vec<SourceChange> = Vec::new();
        for change in batch {
            let change = SourceChange {
                relation: change.table.clone(),
                sign: match change.sign {
                    sqlite_ext::Sign::Insert => Sign::Insert,
                    sqlite_ext::Sign::Delete => Sign::Delete,
                },
                row: change.values.iter().map(catalog::cell_of).collect(),
            };
            // One relation may feed both programs.
            if self.producer.sources.contains(&change.relation) {
                producer_rows.push(change.clone());
            }
            if self.consumer.sources.iter().any(|s| *s == change.relation && *s != self.view) {
                consumer_rows.push(change);
            }
        }
        if producer_rows.is_empty() && consumer_rows.is_empty() {
            return Ok(());
        }
        self.settle(db, producer_rows, consumer_rows)
            .map_err(rusqlite::Error::from)
    }
}

impl GroupTrigger {
    /// Producer first, then the consumer with the producer's net output
    /// delta mapped onto the view relation and the consumer's own rows
    /// appended. Both settle inside the caller's `xSync`: no savepoints, no
    /// virtual-table writes. Any error propagates, failing the whole COMMIT.
    fn settle(
        &self,
        conn: &Connection,
        producer_rows: Vec<SourceChange>,
        mut consumer_rows: Vec<SourceChange>,
    ) -> Result<(), EngineError> {
        let producer_settled = !producer_rows.is_empty();
        if producer_settled {
            let delta = engine::settle(conn, &self.producer, &producer_rows, true)?;
            for change in delta {
                consumer_rows.push(SourceChange {
                    relation: self.view.clone(),
                    sign: change.sign,
                    row: change.row,
                });
            }
        }
        if producer_settled || !consumer_rows.is_empty() {
            engine::settle(conn, &self.consumer, &consumer_rows, true)?;
        }
        Ok(())
    }
}
