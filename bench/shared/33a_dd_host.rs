//! Finite lab circuit catalog. Reuses the store DD oracle's iterate pattern.
use differential_dataflow::input::Input;
use differential_dataflow::VecCollection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use timely::dataflow::operators::probe::Handle;

fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn row(v: &Value) -> [i64; 3] {
    let a = v.as_array().unwrap();
    [
        a[0].as_i64().unwrap(),
        a[1].as_i64().unwrap(),
        a[2].as_i64().unwrap(),
    ]
}
fn canonical(prefix: &str, rows: &[Vec<i64>]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{}\t{}\n",
                prefix,
                r.iter().map(i64::to_string).collect::<Vec<_>>().join("\t")
            )
        })
        .collect()
}
fn unavailable(unit: &str, reason: &str) -> Value {
    json!({"value":null,"unit":unit,"unavailable_reason":reason})
}
fn native_inventory(keyed: &[BTreeMap<i64, [i64; 3]>; 3], output_rows: usize) -> Value {
    let reason="DD arrangement/trace records and heap bytes are not exposed by this adapter; source/output bags are a partial native-state inventory";
    let source_rows = keyed.iter().map(BTreeMap::len).sum::<usize>();
    let mut relations=keyed.iter().enumerate().map(|(i,rows)|{let name=["a","b","c"][i];json!({"name":name,"kind":"native-collection","role":"source","counted_in_totals":true,"row_count":{"value":rows.len(),"unit":"rows","unavailable_reason":null},"bytes":{"allocated":unavailable("bytes",reason),"data":unavailable("bytes",reason),"index":unavailable("bytes",reason)}})}).collect::<Vec<_>>();
    relations.push(json!({"name":"output-bag","kind":"native-collection","role":"result","counted_in_totals":true,"row_count":{"value":output_rows,"unit":"rows","unavailable_reason":null},"bytes":{"allocated":unavailable("bytes",reason),"data":unavailable("bytes",reason),"index":unavailable("bytes",reason)}}));
    json!({"schema_version":1,"measured_at":"after-output-validation","outside_timed_region":true,"scope":"adapter-observed DD input collections and consolidated output bag","relations":relations,"summary":{"table_count":{"value":0,"unit":"tables","unavailable_reason":null},"index_count":unavailable("indexes",reason),"native_collection_count":{"value":4,"unit":"collections","unavailable_reason":null},"total_rows":{"value":source_rows+output_rows,"unit":"rows","unavailable_reason":null,"partial":true},"rows_by_role":{"source":{"value":source_rows,"unit":"rows","unavailable_reason":null},"result":{"value":output_rows,"unit":"rows","unavailable_reason":null},"support":unavailable("rows",reason)},"table_bytes":unavailable("bytes","DD has no SQL tables"),"index_bytes":unavailable("bytes",reason),"total_relation_bytes":unavailable("bytes",reason)},"storage":{"database_file_bytes":unavailable("bytes","volatile DD adapter has no database file"),"wal_file_bytes":unavailable("bytes","volatile DD adapter has no WAL file"),"database_allocated_bytes":unavailable("bytes","volatile DD adapter has no database allocation"),"database_size_scope":"no durable database"},"process_memory":{"rss_bytes":unavailable("bytes","measured by parent runner")},"limitations":[reason]})
}
type Graph = for<'s> fn(
    &str,
    VecCollection<'s, u64, [i64; 3]>,
    VecCollection<'s, u64, [i64; 3]>,
    VecCollection<'s, u64, [i64; 3]>,
) -> VecCollection<'s, u64, Vec<i64>>;
pub fn run(graph: Graph) {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap())
            .unwrap();
    timely::execute_directly(move |worker| {
        let start = Instant::now();
        let output = Arc::new(Mutex::new(BTreeMap::<Vec<i64>, isize>::new()));
        let observed = Arc::new(Mutex::new(BTreeMap::<(usize, [i64; 3]), isize>::new()));
        let mut probe = Handle::new();
        let mut inputs = worker.dataflow::<u64, _, _>(|scope| {
            let (ai, a) = scope.new_collection::<[i64; 3], isize>();
            let (bi, b) = scope.new_collection::<[i64; 3], isize>();
            let (ci, c) = scope.new_collection::<[i64; 3], isize>();
            for (index, collection) in [a.clone(), b.clone(), c.clone()].into_iter().enumerate() {
                let observed = Arc::clone(&observed);
                collection
                    .inspect(move |(r, _, d)| {
                        let mut o = observed.lock().unwrap();
                        let key = (index, *r);
                        *o.entry(key).or_default() += d;
                        if o[&key] == 0 {
                            o.remove(&key);
                        }
                    })
                    .probe_with(&mut probe);
            }
            let result = graph(fixture["circuit"].as_str().unwrap(), a, b, c);
            let out = Arc::clone(&output);
            result
                .consolidate()
                .inspect(move |(r, _, d)| {
                    let mut o = out.lock().unwrap();
                    *o.entry(r.clone()).or_default() += d;
                    if o[r] == 0 {
                        o.remove(r);
                    }
                })
                .probe_with(&mut probe);
            [ai, bi, ci]
        });
        println!(
            "{}",
            json!({"event":"case-setup","status":"ok","setup_ms":start.elapsed().as_secs_f64()*1000.,"algorithm":"native-DD-circuit","durability":"volatile","workers":1,"time_contract":"u64 sequential epochs; all three input frontiers advanced then output probe awaited"})
        );
        // Separate finite frontier check before timed mutations. A retained third
        // input frontier must prevent the combined input/output probe completing.
        for input in &mut inputs[..2] {
            input.advance_to(1);
            input.flush();
        }
        for _ in 0..128 {
            worker.step();
        }
        assert!(probe.less_than(&1));
        inputs[2].advance_to(1);
        inputs[2].flush();
        while probe.less_than(&1) {
            worker.step();
        }
        println!(
            "{}",
            json!({"event":"frontier-check","status":"ok","held_input":"c","epoch":1,"completion_after_all_inputs_advanced":true,"contract":"finite scalar frontier check; no arbitrary partial-order time claim"})
        );
        let mut keyed: [BTreeMap<i64, [i64; 3]>; 3] = Default::default();
        let mut total = 0.;
        let mut last_input = String::new();
        let mut last_output = String::new();
        for (index, state) in fixture["states"].as_array().unwrap().iter().enumerate() {
            let start = Instant::now();
            for write in state["writes"].as_array().unwrap() {
                let table = match write["table"].as_str().unwrap() {
                    "a" => 0,
                    "b" => 1,
                    "c" => 2,
                    _ => panic!("table"),
                };
                let id = write["id"].as_i64().unwrap();
                if let Some(old) = keyed[table].remove(&id) {
                    inputs[table].update(old, -1);
                }
                if !write["row"].is_null() {
                    let r = row(&write["row"]);
                    assert_eq!(r[0], id);
                    assert!(r.iter().all(|n| n.abs() <= 1_000_000));
                    keyed[table].insert(id, r);
                    inputs[table].update(r, 1);
                }
            }
            let epoch = index as u64 + 2;
            for input in &mut inputs {
                input.advance_to(epoch);
                input.flush();
            }
            while probe.less_than(&epoch) {
                worker.step();
            }
            let update = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            let mut rows = Vec::new();
            for (r, d) in output.lock().unwrap().iter() {
                assert!(*d > 0);
                for _ in 0..*d {
                    rows.push(r.clone());
                }
            }
            let compute = start.elapsed().as_secs_f64() * 1000.;
            let expected: Vec<Vec<i64>> =
                serde_json::from_value(state["expected"]["rows"].clone()).unwrap();
            assert_eq!(rows, expected, "{}", state["name"]);
            let mut actual_input = String::new();
            for (table, name) in ["a", "b", "c"].iter().enumerate() {
                let actual: Vec<Vec<i64>> = observed
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|((t, _), _)| *t == table)
                    .map(|((_, r), d)| {
                        assert_eq!(*d, 1);
                        r.to_vec()
                    })
                    .collect();
                let mut expected: Vec<Vec<i64>> =
                    serde_json::from_value(state["inputs"][name].clone()).unwrap();
                expected.sort();
                assert_eq!(actual, expected);
                actual_input.push_str(&canonical(&name.to_uppercase(), &actual));
            }
            last_input = hash(&actual_input);
            last_output = hash(&canonical("S", &rows));
            assert_eq!(last_input, state["input_hash"].as_str().unwrap());
            assert_eq!(last_output, state["expected"]["checksum"].as_str().unwrap());
            total += update + compute;
            let inventory = native_inventory(&keyed, rows.len());
            println!(
                "{}",
                json!({"event":"mutation","status":"ok","state":state["name"],"exact_input_output_validated":true,"input_hash":last_input,"checksum":last_output,"affected_rows":state["writes"].as_array().unwrap().len(),"output_rows":rows.len(),"output_bytes":canonical("S",&rows).len(),"update_transaction_ms":update,"query_compute_ms":compute,"update_plus_query_ms":update+compute,"epoch":epoch,"state_inventory":inventory})
            );
        }
        println!(
            "{}",
            json!({"event":"case-total","status":"ok","update_plus_query_ms":total,"final_input_hash":last_input,"final_checksum":last_output,"disk":{"database_bytes":null,"unavailable_reason":"volatile DD adapter has no database file"}})
        );
    });
}
