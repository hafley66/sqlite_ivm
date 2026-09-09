//! Native DD consumer of the existing nonrecursive crossover fixture.
//! SQL clients submit DML; this arm submits keyed writes, with old-row lookup
//! and signed retraction inside the timer. No expected summary enters dataflow.

use differential_dataflow::input::Input;
use differential_dataflow::operators::CountTotal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use timely::dataflow::operators::probe::Handle;

fn rss_bytes() -> u64 {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage.ru_maxrss as u64 * if cfg!(target_os = "linux") { 1024 } else { 1 }
    }
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn main() {
    let path = std::env::args().nth(1).expect("fixture path required");
    let startup = Instant::now();
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let decode_ms = startup.elapsed().as_secs_f64() * 1000.0;
    timely::execute_directly(move |worker| {
        let setup = Instant::now();
        let observed_fact = Arc::new(Mutex::new(BTreeMap::<(i64, i64, i64), i64>::new()));
        let observed_dimension = Arc::new(Mutex::new(BTreeMap::<(i64, i64), i64>::new()));
        let output = Arc::new(Mutex::new(BTreeMap::<(i64, i64, i64), isize>::new()));
        let mut probe = Handle::new();
        let (mut fact_input, mut dimension_input) = worker.dataflow::<u64, _, _>(|scope| {
            let (fi, facts) = scope.new_collection::<(i64, i64, i64), i64>();
            let (di, dimensions) = scope.new_collection::<(i64, i64), i64>();
            let fact_capture = Arc::clone(&observed_fact);
            facts
                .clone()
                .inspect(move |(row, _, diff)| {
                    let mut rows = fact_capture.lock().unwrap();
                    let weight = rows.entry(*row).or_default();
                    *weight += diff;
                    if *weight == 0 {
                        rows.remove(row);
                    }
                })
                .probe_with(&mut probe);
            let dimension_capture = Arc::clone(&observed_dimension);
            dimensions
                .clone()
                .inspect(move |(row, _, diff)| {
                    let mut rows = dimension_capture.lock().unwrap();
                    let weight = rows.entry(*row).or_default();
                    *weight += diff;
                    if *weight == 0 {
                        rows.remove(row);
                    }
                })
                .probe_with(&mut probe);
            let sink = Arc::clone(&output);
            facts
                .map(|(_id, group, amount)| (group, amount))
                .join(dimensions)
                .explode(|(group, (amount, factor))| {
                    Some((group, (1i64, amount.checked_mul(factor).unwrap())))
                })
                .count_total()
                .map(|(group, (count, sum))| (group, count, sum))
                .consolidate()
                .inspect(move |(row, _, diff)| {
                    let mut rows = sink.lock().unwrap();
                    let weight = rows.entry(*row).or_default();
                    *weight += diff;
                    if *weight == 0 {
                        rows.remove(row);
                    }
                })
                .probe_with(&mut probe);
            (fi, di)
        });
        let graph_ms = setup.elapsed().as_secs_f64() * 1000.0;
        let mut facts = BTreeMap::<i64, (i64, i64, i64)>::new();
        let mut dimensions = BTreeMap::<i64, (i64, i64)>::new();
        let mut total_ms = 0.0;
        let mut final_checksum = String::new();
        let mut final_input_hash = String::new();
        for (index, state) in fixture["states"].as_array().unwrap().iter().enumerate() {
            // Parse command payloads before the write clock, as SQL text and
            // bindings are prepared outside the SQL arms' transaction clocks.
            let writes = &state["keyed_writes"];
            let fact_deletes: Vec<i64> =
                serde_json::from_value(writes["fact"]["deletes"].clone()).unwrap();
            let fact_puts: Vec<(i64, i64, i64)> =
                serde_json::from_value(writes["fact"]["puts"].clone()).unwrap();
            let dimension_deletes: Vec<i64> =
                serde_json::from_value(writes["dimension"]["deletes"].clone()).unwrap();
            let dimension_puts: Vec<(i64, i64)> =
                serde_json::from_value(writes["dimension"]["puts"].clone()).unwrap();
            let started = Instant::now();
            for id in fact_deletes {
                if let Some(old) = facts.remove(&id) {
                    fact_input.update(old, -1);
                }
            }
            for row in fact_puts {
                if let Some(old) = facts.insert(row.0, row) {
                    fact_input.update(old, -1);
                }
                fact_input.update(row, 1);
            }
            for id in dimension_deletes {
                if let Some(old) = dimensions.remove(&id) {
                    dimension_input.update(old, -1);
                }
            }
            for row in dimension_puts {
                if let Some(old) = dimensions.insert(row.0, row) {
                    dimension_input.update(old, -1);
                }
                dimension_input.update(row, 1);
            }
            let frontier = index as u64 + 1;
            fact_input.advance_to(frontier);
            dimension_input.advance_to(frontier);
            fact_input.flush();
            dimension_input.flush();
            worker.step_while(|| probe.less_than(&frontier));
            let maintain_ms = started.elapsed().as_secs_f64() * 1000.0;
            let materialize = Instant::now();
            let snapshot: Vec<_> = output
                .lock()
                .unwrap()
                .iter()
                .map(|(row, weight)| (*row, *weight))
                .collect();
            let output_count = snapshot.len();
            let compute_ms = materialize.elapsed().as_secs_f64() * 1000.0;

            // Independent recomputation and complete observed-input validation
            // occur strictly after maintenance/probe/materialization timing.
            assert!(snapshot
                .iter()
                .all(|(row, weight)| *weight == 1 && row.1 > 0));
            let actual_fact: Vec<_> = observed_fact
                .lock()
                .unwrap()
                .iter()
                .map(|(row, weight)| {
                    assert_eq!(*weight, 1);
                    *row
                })
                .collect();
            let actual_dimension: Vec<_> = observed_dimension
                .lock()
                .unwrap()
                .iter()
                .map(|(row, weight)| {
                    assert_eq!(*weight, 1);
                    *row
                })
                .collect();
            assert_eq!(json!(actual_fact), state["inputs"]["fact"]);
            assert_eq!(json!(actual_dimension), state["inputs"]["dimension"]);
            let dim: BTreeMap<_, _> = actual_dimension.iter().copied().collect();
            let mut recomputed = BTreeMap::<i64, (i64, i64)>::new();
            for &(_, group, amount) in &actual_fact {
                if let Some(factor) = dim.get(&group) {
                    let value = recomputed.entry(group).or_default();
                    value.0 += 1;
                    value.1 = value
                        .1
                        .checked_add(amount.checked_mul(*factor).unwrap())
                        .unwrap();
                }
            }
            let expected: Vec<_> = recomputed
                .into_iter()
                .map(|(group, (count, sum))| (group, count, sum))
                .collect();
            let actual: Vec<_> = snapshot.iter().map(|(row, _)| *row).collect();
            assert_eq!(actual, expected);
            let canonical = actual
                .iter()
                .map(|(g, n, sum)| format!("S\t{g}\t{n}\t{sum}"))
                .collect::<Vec<_>>()
                .join("\n");
            final_checksum = hash(&canonical);
            assert_eq!(
                final_checksum,
                state["expected"]["checksum"].as_str().unwrap()
            );
            let input_text = actual_dimension
                .iter()
                .map(|(g, f)| format!("D\t{g}\t{f}"))
                .chain(
                    actual_fact
                        .iter()
                        .map(|(id, g, a)| format!("F\t{id}\t{g}\t{a}")),
                )
                .collect::<Vec<_>>()
                .join("\n");
            final_input_hash = hash(&input_text);
            assert_eq!(final_input_hash, state["input_hash"].as_str().unwrap());
            if index == 0 {
                println!(
                    "{}",
                    json!({"event":"case-setup", "status":"ok", "setup_ms":graph_ms+maintain_ms,
                    "fixture_decode_ms":decode_ms, "graph_ms":graph_ms, "initial_load_ms":maintain_ms,
                    "runtime":"differential-dataflow 0.25.1 / timely 0.31, one worker",
                    "algorithm":"native DD arranged join + tuple-weight CountTotal", "durability":"volatile memory; no WAL, persistence or reopen guarantee",
                    "memory_scope":"Rust process peak including fixture, keyed input maps, observed input traces, DD and validation", "total_memory_enforcement":"UNENFORCED"})
                );
            } else {
                total_ms += maintain_ms + compute_ms;
            }
            println!(
                "{}",
                json!({"event":"mutation", "status":"ok", "state":state["name"],
                "update_transaction_ms":if index==0 {0.0} else {maintain_ms}, "query_compute_ms":compute_ms,
                "update_plus_query_ms":if index==0 {compute_ms} else {maintain_ms+compute_ms},
                "affected_rows":state["expected_affected_rows"], "affected_rows_scope":"normalized keyed-write request; exact source transitions separately validated",
                "join_affected_rows":state["join_affected_rows"], "input_hash":final_input_hash,
                "checksum":final_checksum, "output_rows":output_count, "output_bytes":canonical.len(),
                "exact_input_output_validated":true, "completed_frontier":frontier, "summary":actual})
            );
        }
        println!(
            "{}",
            json!({"event":"case-total", "status":"ok", "update_plus_query_ms":total_ms,
            "final_checksum":final_checksum, "final_input_hash":final_input_hash,
            "process_peak_rss_bytes":rss_bytes(), "disk":{"database_bytes":0,"scope":"volatile DD state; fixture/log bytes excluded"},
            "fresh_reopen_validated":false})
        );
    });
}
