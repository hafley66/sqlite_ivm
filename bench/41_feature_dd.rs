#[path = "40_feature_graphs.rs"]
mod graphs;
use differential_dataflow::input::Input;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use timely::dataflow::operators::probe::Handle;
fn source(v: &Value) -> graphs::R {
    let r = v.as_array().unwrap();
    (
        r[0].as_i64().unwrap(),
        r[1].as_i64(),
        r[2].as_i64(),
        r.get(3).and_then(Value::as_str).unwrap_or("").to_string(),
    )
}
fn expected(v: &Value) -> Vec<graphs::Row> {
    let mut rows = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r.as_array()
                .unwrap()
                .iter()
                .map(|v| match v {
                    Value::Null => "null".into(),
                    Value::String(s) => format!("s{s}"),
                    Value::Number(n) => {
                        if let Some(n) = n.as_i64() {
                            format!("n{n}")
                        } else {
                            format!("n{}", n.as_f64().unwrap())
                        }
                    }
                    _ => panic!("unsupported fixture value"),
                })
                .collect()
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
fn main() {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap())
            .unwrap();
    timely::execute_directly(move |worker| {
        let family = fixture["case"]["name"].as_str().unwrap();
        let output = Arc::new(Mutex::new(BTreeMap::<graphs::Row, isize>::new()));
        let observed = Arc::new(Mutex::new(BTreeMap::<(usize, graphs::R), isize>::new()));
        let mut probe = Handle::new();
        let (mut inputs, mut unit) = worker.dataflow::<u64, _, _>(|scope| {
            let (ai, a) = scope.new_collection::<graphs::R, isize>();
            let (bi, b) = scope.new_collection::<graphs::R, isize>();
            let (ci, c) = scope.new_collection::<graphs::R, isize>();
            let (ui, u) = scope.new_collection::<(), isize>();
            for (i, input) in [a.clone(), b.clone(), c.clone()].into_iter().enumerate() {
                let observed = observed.clone();
                input
                    .inspect(move |(r, _, d)| {
                        let mut o = observed.lock().unwrap();
                        let key = (i, r.clone());
                        *o.entry(key.clone()).or_default() += d;
                        if o[&key] == 0 {
                            o.remove(&key);
                        }
                    })
                    .probe_with(&mut probe);
            }
            let out = output.clone();
            graphs::graph(family, a, b, c, u)
                .consolidate()
                .inspect(move |(r, _, d)| {
                    let mut o = out.lock().unwrap();
                    *o.entry(r.clone()).or_default() += d;
                    if o[r] == 0 {
                        o.remove(r);
                    }
                })
                .probe_with(&mut probe);
            ([ai, bi, ci], ui)
        });
        unit.insert(());
        unit.advance_to(1);
        unit.flush();
        for input in &mut inputs[..2] {
            input.advance_to(1);
            input.flush();
        }
        for _ in 0..64 {
            worker.step();
        }
        assert!(probe.less_than(&1));
        inputs[2].advance_to(1);
        inputs[2].flush();
        while probe.less_than(&1) {
            worker.step();
        }
        let mut current: [BTreeMap<i64, graphs::R>; 3] = Default::default();
        for (index, state) in fixture["states"].as_array().unwrap().iter().enumerate() {
            for (i, table) in ["a", "b", "c"].iter().enumerate() {
                let next = state["inputs"][table]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        let r = source(r);
                        (r.0, r)
                    })
                    .collect::<BTreeMap<_, _>>();
                for (id, old) in &current[i] {
                    if next.get(id) != Some(old) {
                        inputs[i].update(old.clone(), -1);
                    }
                }
                for (id, row) in &next {
                    if current[i].get(id) != Some(row) {
                        inputs[i].update(row.clone(), 1);
                    }
                }
                current[i] = next;
            }
            let epoch = index as u64 + 2;
            for input in &mut inputs {
                input.advance_to(epoch);
                input.flush();
            }
            unit.advance_to(epoch);
            unit.flush();
            while probe.less_than(&epoch) {
                worker.step();
            }
            let actual_input = observed.lock().unwrap().clone();
            let expected_input = current
                .iter()
                .enumerate()
                .flat_map(|(i, rows)| rows.values().map(move |r| ((i, r.clone()), 1isize)))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(actual_input, expected_input, "{family} input {index}");
            let mut actual = vec![];
            for (row, d) in output.lock().unwrap().iter() {
                assert!(*d > 0);
                for _ in 0..*d {
                    actual.push(row.clone());
                }
            }
            actual.sort();
            assert_eq!(
                actual,
                expected(&state["expected"]),
                "{family} state {index}: {}",
                state["mutation"]
            );
        }
        println!(
            "{}",
            json!({"engine":"dd","case":family,"states":fixture["states"].as_array().unwrap().len(),"status":"ok","input_output_verified":true,"signed_deltas":true,"held_frontier_verified":true})
        );
    });
}
