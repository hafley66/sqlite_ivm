//! The three flush strategies differ only in when the sink is written. Each
//! one owes the same thing: every row lands, once, in the order it was
//! written.

use std::sync::{Arc, Mutex};

use hafley_observe::{Flush, Row, Sink, Writer};

/// Rows per strategy. Below the drain bound, so nothing is dropped.
const ROWS: usize = 1000;

#[derive(Default)]
struct Recorder {
    rows: Mutex<Vec<Row>>,
}

impl Recorder {
    fn landed(&self) -> Vec<Row> {
        self.rows.lock().expect("recorder lock").clone()
    }
}

impl Sink for Recorder {
    fn label(&self) -> &'static str {
        "recorder"
    }

    fn write(&self, rows: &[Row]) {
        self.rows
            .lock()
            .expect("recorder lock")
            .extend_from_slice(rows);
    }
}

fn row(index: usize) -> Row {
    Row {
        ts_ns: index as i64,
        level: "INFO",
        name: "paragraph".to_owned(),
        target: "engine.layout".to_owned(),
        file: "layout.rs".to_owned(),
        line: index as u32,
        fields: vec![("row".to_owned(), index.to_string())],
    }
}

fn writer(recorder: &Arc<Recorder>, strategy: Flush) -> Writer {
    let sink: Arc<dyn Sink> = Arc::clone(recorder) as Arc<dyn Sink>;
    Writer::new(sink, strategy)
}

#[test]
fn every_strategy_delivers_every_row_once_and_in_order() {
    for strategy in [Flush::Immediate, Flush::Drain, Flush::OnCommit] {
        let recorder = Arc::new(Recorder::default());
        let writer = writer(&recorder, strategy);
        for index in 0..ROWS {
            writer.write(row(index));
        }
        writer.flush();

        let landed = recorder.landed();
        assert_eq!(landed.len(), ROWS, "{} lost rows", strategy.as_str());
        assert!(
            landed
                .iter()
                .enumerate()
                .all(|(index, landed)| landed.ts_ns == index as i64),
            "{} reordered rows",
            strategy.as_str()
        );
        assert_eq!(writer.dropped(), 0, "{} dropped rows", strategy.as_str());
    }
}

#[test]
fn a_commit_point_is_what_writes_an_on_commit_sink() {
    let recorder = Arc::new(Recorder::default());
    let writer = writer(&recorder, Flush::OnCommit);
    for index in 0..ROWS {
        writer.write(row(index));
    }
    assert_eq!(recorder.landed().len(), 0, "on-commit wrote before the commit");

    writer.flush();
    assert_eq!(recorder.landed().len(), ROWS, "the commit wrote the buffer");
}

#[test]
fn an_inline_sink_has_already_written_when_the_row_returns() {
    let recorder = Arc::new(Recorder::default());
    let writer = writer(&recorder, Flush::Immediate);
    for index in 0..ROWS {
        writer.write(row(index));
        assert_eq!(recorder.landed().len(), index + 1);
    }
}