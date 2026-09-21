//! One layer turns the tracing stream into rows and hands them to a writer.
//! Every sink in this crate is fed by it, so the priced cost of a sink is the
//! cost of the strategy, not of a second recorder.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::flush::{Row, Writer};

thread_local! {
    /// A writer reports its bound through `tracing`, and that report is an
    /// event. This flag stops it re-entering the recorder that raised it.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        RECORDING.with(|flag| flag.set(false));
    }
}

pub struct SinkLayer {
    writer: Arc<Writer>,
    span_events: bool,
}

impl SinkLayer {
    pub fn new(writer: Arc<Writer>) -> Self {
        Self {
            writer,
            span_events: false,
        }
    }

    pub fn with_span_events(mut self, span_events: bool) -> Self {
        self.span_events = span_events;
        self
    }
}

impl<S> Layer<S> for SinkLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if RECORDING.with(|flag| flag.replace(true)) {
            return;
        }
        let _guard = Guard;
        self.writer.write(row_for(event, &ctx));
    }

    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &tracing::Id, ctx: Context<'_, S>) {
        if !self.span_events {
            return;
        }
        if RECORDING.with(|flag| flag.replace(true)) {
            return;
        }
        let _guard = Guard;
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let meta = span.metadata();
        self.writer.write(Row {
            ts_ns: now_ns(),
            level: meta.level().as_str(),
            name: span.name().to_owned(),
            target: meta.target().to_owned(),
            file: meta.file().unwrap_or_default().to_owned(),
            line: meta.line().unwrap_or_default(),
            fields: fields.into_value(),
        });
    }
}

fn now_ns() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos() as i64,
        Err(_) => 0,
    }
}

fn row_for<S>(event: &Event<'_>, ctx: &Context<'_, S>) -> Row
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let mut fields = Fields::default();
    event.record(&mut fields);
    let meta = event.metadata();
    Row {
        ts_ns: now_ns(),
        level: meta.level().as_str(),
        name: match ctx.lookup_current() {
            Some(span) => span.name().to_owned(),
            None => "event".to_owned(),
        },
        target: meta.target().to_owned(),
        file: meta.file().unwrap_or_default().to_owned(),
        line: meta.line().unwrap_or_default(),
        fields: fields.into_value(),
    }
}

#[derive(Default)]
struct Fields {
    values: Vec<(String, String)>,
}

impl Fields {
    fn into_value(self) -> Vec<(String, String)> {
        self.values
    }

    fn push(&mut self, field: &Field, value: String) {
        self.values.push((field.name().to_owned(), value));
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_owned());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }
}