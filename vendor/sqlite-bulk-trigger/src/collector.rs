use crate::schema;
use rusqlite::{types::Value, Connection, Result};

/// Rows held in memory before the collector spills to its shadow table.
/// Protects the process from one transaction that inserts a whole file.
pub const STAGED_ROWS: usize = 10_000;

/// Bytes of payload held in memory before spilling. Same protection, for wide rows.
pub const STAGED_BYTES: usize = 8 << 20;

/// Direction of a row-level event. An UPDATE arrives as `Delete` of the old
/// image then `Insert` of the new image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    Insert,
    Delete,
}

impl Sign {
    /// The integer a source trigger writes into the collector's `__sign` column.
    pub const fn as_integer(self) -> i64 {
        match self {
            Sign::Insert => 1,
            Sign::Delete => -1,
        }
    }

    pub fn from_integer(value: i64) -> Option<Self> {
        match value {
            1 => Some(Sign::Insert),
            -1 => Some(Sign::Delete),
            _ => None,
        }
    }
}

/// One row-level event, captured in xUpdate, delivered in a batch.
#[derive(Clone, Debug, PartialEq)]
pub struct RowChange {
    pub table: String,
    pub sign: Sign,
    /// New image for `Insert`, old image for `Delete`, in declared column order.
    pub values: Vec<Value>,
    /// Position within the transaction, monotone from 0. `ROLLBACK TO` restores
    /// the counter, so a delivered batch carries contiguous numbers.
    pub sequence: u64,
}

impl RowChange {
    /// `sequence` is stamped by [`Collector::update`], so it starts at 0 here.
    pub fn new(table: impl Into<String>, sign: Sign, values: Vec<Value>) -> Self {
        Self {
            table: table.into(),
            sign,
            values,
            sequence: 0,
        }
    }
}

/// Called once per transaction at xSync with every surviving change in sequence
/// order. Ordinary-table writes are legal inside it; an empty batch is skipped.
pub trait BulkTrigger: 'static {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> Result<()>;
}

/// Times SQLite entered each collector callback since the last reset.
/// `watch` zeroes it after its own DDL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub begin: u64,
    pub savepoint: u64,
    pub release: u64,
    pub rollback_to: u64,
    pub update: u64,
    pub sync: u64,
    pub commit: u64,
    pub rollback: u64,
}

/// Position the collector returns to when SQLite rolls back to `savepoint`.
#[derive(Clone, Copy, Debug)]
struct Mark {
    savepoint: i32,
    spilled_rows: usize,
    next_sequence: u64,
    /// Drains before the mark. A drain numbered at or past this wrote to disk
    /// after the savepoint, so ROLLBACK TO unwinds its rows.
    drains: u64,
}

/// The batch state machine, with its own spill storage and no virtual table.
///
/// @comment-ok: the call order below is the type's whole contract.
///
/// A host that already owns a virtual table and its triggers forwards its
/// callbacks here: [`begin`](Collector::begin) at xBegin,
/// [`update`](Collector::update) per row at xUpdate,
/// [`savepoint`](Collector::savepoint), [`release`](Collector::release) and
/// [`rollback_to`](Collector::rollback_to) at the savepoint trio,
/// [`drain`](Collector::drain) at xSync, then [`commit`](Collector::commit) or
/// [`rollback`](Collector::rollback). [`watch`](crate::watch) is the standalone
/// path over this same type.
pub struct Collector {
    name: String,
    width: usize,
    counts: Counts,
    staged: Vec<RowChange>,
    staged_bytes: usize,
    spilled_rows: usize,
    marks: Vec<Mark>,
    next_sequence: u64,
    /// Memory rows a drain delivered while a savepoint was open, tagged with
    /// the drain number. Empty whenever no savepoint is open.
    applied: Vec<(RowChange, u64)>,
    drains: u64,
    staged_row_cap: usize,
    staged_byte_cap: usize,
}

impl Collector {
    /// `width` is the widest watched table's column count, which sizes the
    /// shadow table. A row wider than that is refused by [`Collector::update`].
    pub fn new(name: impl Into<String>, width: usize) -> Self {
        Self {
            name: name.into(),
            width,
            counts: Counts::default(),
            staged: Vec::new(),
            staged_bytes: 0,
            spilled_rows: 0,
            marks: Vec::new(),
            next_sequence: 0,
            applied: Vec::new(),
            drains: 0,
            staged_row_cap: STAGED_ROWS,
            staged_byte_cap: STAGED_BYTES,
        }
    }

    pub fn staged_rows(mut self, rows: usize) -> Self {
        self.staged_row_cap = rows.max(1);
        self
    }

    pub fn staged_bytes(mut self, bytes: usize) -> Self {
        self.staged_byte_cap = bytes.max(1);
        self
    }

    /// Name of the shadow table spilled rows land in.
    pub fn shadow_table(&self) -> String {
        schema::delta_name(&self.name)
    }

    /// Follow a shadow-table rename performed by the host. Pending rows and
    /// savepoint marks stay intact. After a DDL rollback the host supplies the
    /// restored owner name before updating or draining the collector again.
    pub fn rebind_shadow(&mut self, owner: impl Into<String>) {
        self.name = owner.into();
    }

    pub fn create_shadow(&self, db: &Connection) -> Result<()> {
        db.execute_batch(&schema::create_delta(&self.name, self.width))
    }

    pub fn drop_shadow(&self, db: &Connection) -> Result<()> {
        db.execute_batch(&format!(
            "DROP TABLE IF EXISTS main.{}",
            schema::quote(&self.shadow_table())
        ))
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn reset_counts(&mut self) {
        self.counts = Counts::default();
    }

    /// Rows of the open transaction still held in memory.
    pub fn staged(&self) -> &[RowChange] {
        &self.staged
    }

    /// Rows of the open transaction already written to the shadow table.
    pub fn spilled_rows(&self) -> usize {
        self.spilled_rows
    }

    pub fn begin(&mut self) {
        self.counts.begin += 1;
        self.reset();
    }

    /// Numbers the change and either stages it or writes it to the shadow
    /// table. The `sequence` the caller supplied is overwritten.
    pub fn update(&mut self, db: &Connection, change: RowChange) -> Result<()> {
        self.counts.update += 1;
        if change.values.len() > self.width {
            return Err(schema::error(format!(
                "a {}-column row does not fit a collector of width {}",
                change.values.len(),
                self.width
            )));
        }
        let mut change = change;
        change.sequence = self.next_sequence;
        self.next_sequence += 1;
        let bytes = row_bytes(&change);
        let room = self.staged.len() < self.staged_row_cap
            && self.staged_bytes + bytes <= self.staged_byte_cap;
        if room {
            self.staged_bytes += bytes;
            self.staged.push(change);
            return Ok(());
        }
        self.spilled_rows += 1;
        self.write_shadow(db, &change)
    }

    pub fn savepoint(&mut self, savepoint: i32) {
        self.counts.savepoint += 1;
        // SQLite numbers savepoints as a stack, so a repeat of an index retires
        // the older mark at that index.
        self.marks.retain(|mark| mark.savepoint < savepoint);
        self.marks.push(Mark {
            savepoint,
            spilled_rows: self.spilled_rows,
            next_sequence: self.next_sequence,
            drains: self.drains,
        });
    }

    /// RELEASE invalidates the named savepoint and everything inside it.
    pub fn release(&mut self, savepoint: i32) {
        self.counts.release += 1;
        self.marks.retain(|mark| mark.savepoint < savepoint);
        if self.marks.is_empty() {
            self.applied.clear();
        }
    }

    /// ROLLBACK TO leaves the named savepoint open, so its mark survives. No
    /// mark at that index means the savepoint predates the first write here.
    /// Memory rows a later drain wrote return to `staged`; the shadow table
    /// unwinds with the page on its own.
    pub fn rollback_to(&mut self, savepoint: i32) {
        self.counts.rollback_to += 1;
        let restored = self
            .marks
            .iter()
            .rposition(|mark| mark.savepoint == savepoint)
            .map(|at| self.marks[at]);
        self.marks.retain(|mark| mark.savepoint <= savepoint);
        let Some(mark) = restored else {
            self.reset();
            return;
        };
        let mut staged = std::mem::take(&mut self.staged);
        staged.retain(|change| change.sequence < mark.next_sequence);
        let (unwound, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.applied)
            .into_iter()
            .partition(|(_, drain)| *drain >= mark.drains);
        self.applied = kept;
        staged.extend(
            unwound
                .into_iter()
                .map(|(change, _)| change)
                .filter(|change| change.sequence < mark.next_sequence),
        );
        staged.sort_by_key(|change| change.sequence);
        self.staged_bytes = staged.iter().map(row_bytes).sum();
        self.staged = staged;
        self.spilled_rows = mark.spilled_rows;
        self.next_sequence = mark.next_sequence;
    }

    /// Every surviving change in sequence order, memory rows merged with the
    /// shadow table's. The shadow table is emptied. Open savepoints stay open;
    /// a later ROLLBACK TO can hand the memory rows back to `staged`.
    pub fn drain(&mut self, db: &Connection) -> Result<Vec<RowChange>> {
        self.counts.sync += 1;
        let staged = std::mem::take(&mut self.staged);
        let spilled = self.spilled_rows;
        self.staged_bytes = 0;
        self.spilled_rows = 0;
        if !self.marks.is_empty() {
            let drain = self.drains;
            self.applied
                .extend(staged.iter().cloned().map(|change| (change, drain)));
        }
        self.drains += 1;
        if spilled == 0 {
            return Ok(staged);
        }
        let batch = merge(staged, self.read_shadow(db)?);
        db.prepare_cached(&schema::delete_delta(&self.name))?
            .execute([])?;
        Ok(batch)
    }

    /// SQLite discards xCommit's return code, so a leftover batch is reported
    /// and never raised.
    pub fn commit(&mut self) {
        self.counts.commit += 1;
        let (staged, spilled) = (self.staged.len(), self.spilled_rows);
        if staged != 0 || spilled != 0 {
            tracing::error!(staged, spilled, "the collector reached commit undrained");
        }
    }

    pub fn rollback(&mut self) {
        self.counts.rollback += 1;
        self.reset();
    }

    fn write_shadow(&self, db: &Connection, change: &RowChange) -> Result<()> {
        let mut parameters = Vec::with_capacity(self.width + 4);
        parameters.push(Value::Integer(change.sequence as i64));
        parameters.push(Value::Text(change.table.clone()));
        parameters.push(Value::Integer(change.sign.as_integer()));
        parameters.push(Value::Integer(change.values.len() as i64));
        parameters.extend(change.values.iter().cloned());
        parameters.resize(self.width + 4, Value::Null);
        db.prepare_cached(&schema::insert_delta(&self.name, self.width))?
            .execute(rusqlite::params_from_iter(parameters))?;
        Ok(())
    }

    fn read_shadow(&self, db: &Connection) -> Result<Vec<RowChange>> {
        let mut statement = db.prepare_cached(&schema::select_delta(&self.name, self.width))?;
        let rows = statement
            .query_map([], |row| {
                let sequence: i64 = row.get(0)?;
                let table: String = row.get(1)?;
                let code: i64 = row.get(2)?;
                let arity: i64 = row.get(3)?;
                let sign = Sign::from_integer(code)
                    .ok_or_else(|| schema::error(format!("shadow row carries sign {code}")))?;
                let values = (0..arity as usize)
                    .map(|at| row.get::<_, Value>(4 + at))
                    .collect::<Result<Vec<_>>>()?;
                Ok(RowChange {
                    table,
                    sign,
                    values,
                    sequence: sequence as u64,
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn reset(&mut self) {
        self.staged.clear();
        self.staged_bytes = 0;
        self.spilled_rows = 0;
        self.marks.clear();
        self.applied.clear();
        self.drains = 0;
        self.next_sequence = 0;
    }
}

/// Interleaves the memory half and the shadow-table half of one batch. Both
/// arrive sorted by sequence.
fn merge(staged: Vec<RowChange>, spilled: Vec<RowChange>) -> Vec<RowChange> {
    let mut merged = Vec::with_capacity(staged.len() + spilled.len());
    let mut staged = staged.into_iter().peekable();
    let mut spilled = spilled.into_iter().peekable();
    // Bound: staged.len() + spilled.len(). Each step moves one row out of one
    // of two finite vectors and neither is refilled.
    while staged.peek().is_some() || spilled.peek().is_some() {
        let from_staged = match (staged.peek(), spilled.peek()) {
            (Some(left), Some(right)) => left.sequence <= right.sequence,
            (Some(_), None) => true,
            _ => false,
        };
        let next = if from_staged {
            staged.next()
        } else {
            spilled.next()
        };
        merged.extend(next);
    }
    merged
}

fn row_bytes(change: &RowChange) -> usize {
    std::mem::size_of::<RowChange>()
        + change.table.len()
        + change.values.iter().map(value_bytes).sum::<usize>()
}

fn value_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>()
        + match value {
            Value::Text(text) => text.len(),
            Value::Blob(bytes) => bytes.len(),
            _ => 0,
        }
}
