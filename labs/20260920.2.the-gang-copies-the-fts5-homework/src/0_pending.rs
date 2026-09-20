use std::ffi::c_int;

/// One staged source row: the sign the source trigger carried, plus the image
/// columns the maintenance query reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Staged {
    pub sign: i64,
    pub group: i64,
    pub value: i64,
}

pub const STAGED_BYTES: usize = std::mem::size_of::<Staged>();

/// Caps resident buffer memory between flushes. One transaction can touch every
/// row of every source, so without a cap the buffer follows the transaction size.
pub const PENDING_BYTE_CAP: usize = 1 << 20;

/// Caps outstanding savepoint marks. Policy::Mark holds one buffer segment per
/// mark, so resident memory stays under (ceiling + 1) * cap.
pub const PENDING_MARK_CEILING: usize = 32;

pub const PENDING_CAP_DIAGNOSTIC: &str = "pending buffer reached its byte cap, flushing early";

pub const PENDING_MARK_DIAGNOSTIC: &str =
    "pending buffer reached its savepoint-mark ceiling, flushing through the marks";

pub const PENDING_SPILL_DIAGNOSTIC: &str =
    "rollback to a savepoint the pending buffer already flushed through";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// FTS5's answer, fts5.c:263304 and :263321 and :263339. A destructively
    /// merged hash cannot be partially undone, so savepoints become flush points.
    Flush,
    /// Marks the buffer instead of flushing it. An append-only Vec can be
    /// truncated back, which FTS5's pending hash cannot.
    Mark,
}

pub struct Pending {
    rows: Vec<Staged>,
    marks: Vec<(c_int, usize)>,
    cap: usize,
    spilled: bool,
}

impl Pending {
    pub fn with_cap(cap: usize) -> Self {
        Pending {
            rows: Vec::new(),
            marks: Vec::new(),
            cap: cap.max(STAGED_BYTES),
            spilled: false,
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn marks(&self) -> usize {
        self.marks.len()
    }

    /// Rows below this index belong to an open savepoint. A cap-forced flush must
    /// leave them buffered, or a later rollback-to would erase them from disk.
    pub fn floor(&self) -> usize {
        self.marks.last().map(|(_, at)| *at).unwrap_or(0)
    }

    /// Appends one row and reports whether the cap now obliges the caller to spill.
    pub fn push(&mut self, row: Staged) -> bool {
        self.rows.push(row);
        (self.rows.len() - self.floor()) * STAGED_BYTES >= self.cap
    }

    /// Drains only what a cap-forced flush may safely write.
    pub fn spill(&mut self) -> Vec<Staged> {
        let floor = self.floor();
        self.rows.split_off(floor)
    }

    pub fn take(&mut self) -> Vec<Staged> {
        self.marks.clear();
        std::mem::take(&mut self.rows)
    }

    pub fn discard(&mut self) {
        self.marks.clear();
        self.rows.clear();
        self.spilled = false;
    }

    /// Records the buffer length at savepoint `index` and reports whether the
    /// mark ceiling is now breached.
    pub fn mark(&mut self, index: c_int) -> bool {
        self.marks.retain(|(at, _)| *at < index);
        self.marks.push((index, self.rows.len()));
        self.marks.len() > PENDING_MARK_CEILING
    }

    pub fn release(&mut self, index: c_int) {
        self.marks.retain(|(at, _)| *at <= index);
    }

    /// Truncates back to savepoint `index`. Err means the cap already flushed
    /// rows that rollback-to will erase, so the caller must fail loudly.
    pub fn rewind(&mut self, index: c_int) -> Result<(), &'static str> {
        let recorded = self
            .marks
            .iter()
            .find(|(at, _)| *at == index)
            .map(|(_, len)| *len);
        self.marks.retain(|(at, _)| *at <= index);
        match recorded {
            Some(len) if len <= self.rows.len() => {
                self.rows.truncate(len);
                Ok(())
            }
            None if !self.spilled => {
                self.rows.clear();
                Ok(())
            }
            _ => Err(PENDING_SPILL_DIAGNOSTIC),
        }
    }

    /// Clears the marks so a flush may write through them, at the price of a
    /// later rollback-to below those marks no longer being satisfiable.
    pub fn flush_through_marks(&mut self) {
        self.marks.clear();
        self.spilled = true;
    }
}
