//! Cost per node kind per drain: `cargo run -q --example 6_costs`.
//! Load is the `8_group_limit` shape over `VIEWS` views, 8 shapes, autocommit.
use hafley_observe::sqlite::{query_plan, SQLITE_TARGET};
use hafley_observe::{CountRecorder, EventSums};
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use tracing_subscriber::prelude::*;

/// Every captured statement keeps its SQL text in memory, so the view count
/// is held under the fixture's 20.
const VIEWS: usize = 6;

/// Statements shown with their query plan, largest vm_step first.
const TOP: usize = 4;

const SHAPES: [&[i64]; 8] = [
    &[1, 1, 1],
    &[3, 1, 1],
    &[1, 3, 1],
    &[5, 5, 5],
    &[7, 2, 1],
    &[1, 1, 9],
    &[4, 4, 1],
    &[11, 1, 2],
];

fn seeded(db: &Connection, shape: &[i64]) -> Result<()> {
    db.execute_batch("DELETE FROM a")?;
    let mut id = 0i64;
    for (value, copies) in shape.iter().enumerate() {
        for _ in 0..*copies {
            id += 1;
            db.execute("INSERT INTO a(id,v) VALUES(?1,?2)", (id, value as i64))?;
        }
    }
    Ok(())
}

fn load(db: &Connection) -> Result<()> {
    for installed in 0..VIEWS {
        let limit = installed as i64 % 5 + 1;
        let offset = installed as i64 % 4;
        let query =
            format!("SELECT v AS value FROM a ORDER BY value LIMIT {limit} OFFSET {offset}");
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE g{installed} USING sqlite_ivm('{query}')"
        ))?;
        for shape in SHAPES {
            seeded(db, shape)?;
            db.prepare(&format!("SELECT value FROM g{installed}"))?
                .query_map([], |r| r.get::<_, i64>(0))?
                .count();
        }
    }
    Ok(())
}

fn row(label: &str, spans: usize, sums: &EventSums) {
    let spans = spans.max(1) as f64;
    println!(
        "{:<14} {:>7} {:>10} {:>12} {:>9.1} {:>10.1} {:>9.1}",
        label,
        spans,
        sums.events,
        sums.sum_of("vm_step"),
        sums.sum_of("nanos") / 1e6,
        sums.events as f64 / spans,
        sums.sum_of("nanos") / 1e3 / spans,
    );
}

fn main() -> Result<()> {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    let db = Connection::open_in_memory()?;
    register(&db)?;
    hafley_observe::sqlite::instrument(&db);
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);",
    )?;
    let started = std::time::Instant::now();
    tracing::subscriber::with_default(subscriber, || load(&db))?;
    let wall = started.elapsed();
    hafley_observe::sqlite::silence(&db);

    let drains = recorder.counts().instances_of("drain");
    let spans_by_kind = recorder.span_counts_by_field("node", "kind");
    let by_kind =
        recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "node", "kind", None);
    println!(
        "{:<14} {:>7} {:>10} {:>12} {:>9} {:>10} {:>9}",
        "kind", "spans", "statements", "vm_step", "ms", "stmt/span", "us/span"
    );
    let mut rows = by_kind.iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.sum_of("nanos").total_cmp(&a.1.sum_of("nanos")));
    let mut total = EventSums::default();
    for ((kind, _), sums) in rows {
        let label = if kind.is_empty() { "outside_drain" } else { kind };
        let spans = spans_by_kind.get(kind).copied().unwrap_or_default();
        row(label, spans, sums);
        total.events += sums.events;
        for (name, value) in &sums.sums {
            *total.sums.entry(name.clone()).or_default() += value;
        }
    }
    row("drain", drains, &total);
    println!(
        "wall {:.1} ms, sqlite {:.1} ms",
        wall.as_secs_f64() * 1e3,
        total.sum_of("nanos") / 1e6
    );

    let by_statement =
        recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "node", "kind", Some("sql"));
    let mut statements = by_statement.iter().collect::<Vec<_>>();
    statements.sort_by(|a, b| b.1.sum_of("vm_step").total_cmp(&a.1.sum_of("vm_step")));
    println!();
    println!("top statements by vm_step");
    for ((kind, sql), sums) in statements.iter().take(TOP) {
        let runs = sums.events.max(1) as f64;
        println!(
            "[{kind}] runs {} vm_step/run {:.0} us/run {:.1}",
            sums.events,
            sums.sum_of("vm_step") / runs,
            sums.sum_of("nanos") / 1e3 / runs,
        );
        println!("  {}", sql.split_whitespace().collect::<Vec<_>>().join(" "));
        match query_plan(&db, sql) {
            Ok(plan) => {
                for line in plan {
                    println!("    {line}");
                }
            }
            Err(e) => println!("    plan unavailable: {e}"),
        }
    }
    Ok(())
}
