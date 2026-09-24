//! Cross-program composition: one producer frontier view consumed by a
//! second frontier view.
//!
//! The consumer's only relation is the producer's visible view
//! (`frontier_<producer>`); its compile resolves the view's columns, its
//! install builds its own engine objects, and its settle reads nothing but
//! signed net rows: the producer's settled [`OutputChange`] delta maps 1:1
//! onto the consumer's `SourceChange` batch (relation = the producer's view,
//! row = the producer's output schema). Both programs settle inside one
//! source transaction, producer first, wrapped in one savepoint, so a
//! failure at either level leaves the previous committed state everywhere.
//!
//! Composed programs install without a commit collector. A view carries no
//! AFTER triggers, and even watching the producer's engine tables cannot
//! work: their writes land inside the producer collector's `xSync`, where
//! SQLite forbids virtual-table writes (`SQLITE_LOCKED`). Settlement is
//! therefore explicit: the caller applies source rows, then settles both
//! programs with one [`Composition::settle`] call. SQL-write auto-settlement
//! for composed pairs needs a shared per-group collector and is not built.

use crate::catalog;
use crate::error::{EngineError, Stage};
use crate::observe;
use crate::{Frontier, OutputChange, Program, SourceChange};
use sqlite_ext::rusqlite::{self, Connection};

/// One producer frontier view and the consumer frontier view over it.
///
/// All state lives in the connection's schema, so the pair survives handle
/// drops ([`Composition::open`]), transaction rollbacks, and reopen.
pub struct Composition {
    producer: Program,
    consumer: Program,
}

/// The net signed output change of one composed frontier, per program.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionDelta {
    /// The producer's net output change.
    pub producer: Vec<OutputChange>,
    /// The consumer's net output change over the producer's output.
    pub consumer: Vec<OutputChange>,
}

impl std::fmt::Debug for Composition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composition")
            .field("producer", &self.producer.name())
            .field("consumer", &self.consumer.name())
            .finish()
    }
}

impl Composition {
    /// Install both programs without collectors and validate the edge: the
    /// producer must read no program view and the consumer must read exactly
    /// the producer's view. Any failure tears back what was installed.
    pub fn install(
        conn: &Connection,
        producer: (&str, &str),
        consumer: (&str, &str),
    ) -> Result<Self, EngineError> {
        let producer = Program::install_unwatched(conn, producer.0, producer.1)?;
        // Reject a derived producer before the consumer compiles against it.
        if let Err(e) = Self::validate_producer(conn, &producer) {
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
        if let Err(e) = Self::validate_pair(&producer, &consumer) {
            let _ = consumer.teardown(conn);
            let _ = producer.teardown(conn);
            return Err(e);
        }
        Ok(Self { producer, consumer })
    }

    /// Reload both programs from the connection's catalog and re-validate the
    /// edge against the stored SQL.
    pub fn open(conn: &Connection, producer: &str, consumer: &str) -> Result<Self, EngineError> {
        let producer = Program::open(conn, producer)?;
        let consumer = Program::open(conn, consumer)?;
        Self::validate_chain(conn, &producer, &consumer)?;
        Ok(Self { producer, consumer })
    }

    /// Assemble from existing handles. Structural edge check only: use
    /// [`Composition::install`] or [`Composition::open`] for the full
    /// validation that the producer reads no program view.
    pub fn new(producer: Program, consumer: Program) -> Result<Self, EngineError> {
        Self::validate_pair(&producer, &consumer)?;
        Ok(Self { producer, consumer })
    }

    /// The producer program: settles the caller's source batch first.
    pub fn producer(&self) -> &Program {
        &self.producer
    }

    /// The consumer program: settles from the producer's net output change.
    pub fn consumer(&self) -> &Program {
        &self.consumer
    }

    /// Settle one complete source batch through both programs, producer
    /// first, inside the caller's transaction. Atomic as a unit: a failure at
    /// either level rolls both programs back to the previous committed state.
    pub fn settle(
        &self,
        conn: &Connection,
        batch: &[SourceChange],
    ) -> Result<CompositionDelta, EngineError> {
        let span = tracing::info_span!(
            target: observe::TARGET,
            observe::SETTLE_SPAN,
            program = %self.producer.name(),
            consumer = %self.consumer.name(),
        );
        let _guard = span.enter();
        conn.execute_batch("SAVEPOINT frontier_sp_compose;")
            .map_err(|e| fail("savepoint", self.producer.name(), e))?;
        match self.settle_inner(conn, batch) {
            Ok(delta) => {
                conn.execute_batch("RELEASE frontier_sp_compose;")
                    .map_err(|e| fail("release", self.producer.name(), e))?;
                Ok(delta)
            }
            Err(e) => {
                conn.execute_batch("ROLLBACK TO frontier_sp_compose; RELEASE frontier_sp_compose;")
                    .map_err(|e| fail("rollback", self.producer.name(), e))?;
                Err(e)
            }
        }
    }

    /// Drop both programs' objects, consumer first. Source tables and rows
    /// are left untouched.
    pub fn teardown(self, conn: &Connection) -> Result<(), EngineError> {
        self.consumer.teardown(conn)?;
        self.producer.teardown(conn)
    }

    fn settle_inner(
        &self,
        conn: &Connection,
        batch: &[SourceChange],
    ) -> Result<CompositionDelta, EngineError> {
        let producer_delta = self.producer.settle(conn, batch)?;
        // The producer's output schema is the view's declared column order,
        // so each net output row maps 1:1 onto a source change for the view.
        let consumer_batch: Vec<SourceChange> = producer_delta
            .iter()
            .map(|change| SourceChange {
                relation: self.producer.view(),
                sign: change.sign,
                row: change.row.clone(),
            })
            .collect();
        let consumer_delta = self.consumer.settle(conn, &consumer_batch)?;
        Ok(CompositionDelta {
            producer: producer_delta,
            consumer: consumer_delta,
        })
    }

    fn validate_pair(producer: &Program, consumer: &Program) -> Result<(), EngineError> {
        let view = producer.view();
        let sources = consumer.sources();
        if sources.len() != 1 || sources[0] != view {
            return Err(EngineError::unsupported(
                Stage::Install,
                consumer.name(),
                "the consumer must read exactly one relation: the producer's output view",
            ));
        }
        Ok(())
    }

    fn validate_producer(conn: &Connection, producer: &Program) -> Result<(), EngineError> {
        for source in producer.sources() {
            if catalog::derived_view_program(conn, source)?.is_some() {
                return Err(EngineError::unsupported(
                    Stage::Install,
                    source,
                    "a program view is not a settlement source; only single-level composition is supported",
                ));
            }
        }
        Ok(())
    }

    fn validate_chain(
        conn: &Connection,
        producer: &Program,
        consumer: &Program,
    ) -> Result<(), EngineError> {
        Self::validate_pair(producer, consumer)?;
        Self::validate_producer(conn, producer)
    }
}

fn fail(what: &str, object: &str, e: rusqlite::Error) -> EngineError {
    EngineError::new(
        Stage::Settle,
        object,
        crate::ErrorKind::Sqlite(format!("{what}: {e}")),
    )
}
