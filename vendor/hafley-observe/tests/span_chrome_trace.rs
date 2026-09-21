#![cfg(feature = "chrome")]

use std::fs;
use std::path::PathBuf;
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::prelude::*;

#[test]
fn nested_spans_land_in_chrome_trace() {
    let trace_dir = std::env::temp_dir().join(format!("hafley-observe-chrome-{}", std::process::id()));
    let trace_path: PathBuf = trace_dir.join("trace.json");
    fs::create_dir_all(&trace_dir).unwrap();

    let (chrome_layer, guard) = ChromeLayerBuilder::new().file(&trace_path).build();
    let subscriber = tracing_subscriber::registry().with(chrome_layer);
    tracing::subscriber::with_default(subscriber, || {
        let populate = tracing::debug_span!("populate", rows = 3);
        let _populate_guard = populate.enter();
        for row in 0..3 {
            let maintain = tracing::debug_span!("maintain", row = row);
            let _maintain_guard = maintain.enter();
        }
    });
    drop(guard);

    let raw = fs::read_to_string(&trace_path).unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();

    let mut populate_bounds: Option<(f64, f64)> = None;
    let mut maintain_events: Vec<(char, f64)> = Vec::new();
    for event in &events {
        let name = event["name"].as_str().unwrap_or_default();
        let Some(ts) = event["ts"].as_f64() else {
            continue;
        };
        let phase = event["ph"].as_str().unwrap_or_default();
        if name.contains("populate") {
            if phase == "B" {
                populate_bounds = Some((ts, ts));
            } else if phase == "E" {
                populate_bounds = populate_bounds.map(|(start, _)| (start, ts));
            }
        }
        if name.contains("maintain") && (phase == "B" || phase == "E") {
            let phase_char = phase.chars().next().unwrap();
            maintain_events.push((phase_char, ts));
        }
    }

    let (populate_start, populate_end) = populate_bounds.expect("populate begin and end expected");
    assert_eq!(maintain_events.len(), 6, "three maintain begin and end pairs expected");
    for (phase, ts) in maintain_events {
        assert!(
            ts >= populate_start && ts <= populate_end,
            "maintain {phase} event at {ts} must fall inside populate [{populate_start}, {populate_end}]"
        );
    }

    fs::remove_dir_all(&trace_dir).ok();
}
