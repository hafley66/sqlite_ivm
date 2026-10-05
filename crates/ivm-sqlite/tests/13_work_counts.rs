//! Work counts (`Sqlite::install_counted`) of the registry build's three installs and the c15 IR:
//! a change in the work an install does shows up as a count change here. Uncached installs only:
//! the `image` feature replaces the shard DDL with an image load.
#![cfg(not(feature = "image"))]

use ivm_engine::Engine;
use ivm_ir::{Frontier, Program};
use ivm_sqlite::Sqlite;

fn corpora() -> [(&'static str, Program); 4] {
    [
        ("macro_library", include_str!("corpus/install/1_macro_library.json")),
        ("macrotime", include_str!("corpus/install/2_macrotime.json")),
        ("registry_comptime", include_str!("corpus/install/3_registry_comptime.json")),
        ("c15", include_str!("corpus/8_c15_program.json")),
    ]
    .map(|(name, text)| (name, serde_json::from_str::<Program>(text).unwrap()))
}

#[test]
fn registry_install_work() {
    let mut lines = Vec::new();
    for (name, program) in corpora() {
        let mut counted = Sqlite::install_counted(&program).unwrap();
        let install = counted.work().unwrap();
        counted.settle(Frontier { changes: vec![] }).unwrap();
        let settle = counted.work().unwrap().since(&install);
        let plain = Sqlite::install(&program).unwrap();
        assert_eq!(counted.statements(), plain.statements(), "{name}: counting changed the plan");
        assert_eq!(plain.work(), None);
        lines.push(format!("{name} install {install:?}"));
        lines.push(format!("{name} empty settle {settle:?}"));
    }
    let got = lines.join("\n");
    assert_eq!(got, concat!(
        "macro_library install Work { creates: 999, schema_rows_parsed: 58028, prepared: 1097, prepared_bytes: 278478, unfolded_bytes: 278478, executions: 1097, zero_row_executions: 17, vm_steps: 30402, fullscan_steps: 42, sorts: 120, autoindexes: 0, reprepares: 0, rows_returned: 52, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 10, written_term: 39 }\n",
        "macro_library empty settle Work { creates: 0, schema_rows_parsed: 0, prepared: 16, prepared_bytes: 1291, unfolded_bytes: 1291, executions: 16, zero_row_executions: 7, vm_steps: 150, fullscan_steps: 0, sorts: 4, autoindexes: 0, reprepares: 0, rows_returned: 4, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 1, written_term: 0 }\n",
        "macrotime install Work { creates: 1011, schema_rows_parsed: 59499, prepared: 1125, prepared_bytes: 283414, unfolded_bytes: 283414, executions: 1125, zero_row_executions: 20, vm_steps: 31082, fullscan_steps: 48, sorts: 120, autoindexes: 0, reprepares: 0, rows_returned: 58, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 10, written_term: 49 }\n",
        "macrotime empty settle Work { creates: 0, schema_rows_parsed: 0, prepared: 16, prepared_bytes: 1291, unfolded_bytes: 1291, executions: 16, zero_row_executions: 7, vm_steps: 150, fullscan_steps: 0, sorts: 4, autoindexes: 0, reprepares: 0, rows_returned: 4, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 1, written_term: 0 }\n",
        "registry_comptime install Work { creates: 2953, schema_rows_parsed: 502567, prepared: 3068, prepared_bytes: 1135632, unfolded_bytes: 1135632, executions: 3068, zero_row_executions: 21, vm_steps: 87047, fullscan_steps: 48, sorts: 118, autoindexes: 0, reprepares: 0, rows_returned: 60, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 8, written_term: 49 }\n",
        "registry_comptime empty settle Work { creates: 0, schema_rows_parsed: 0, prepared: 19, prepared_bytes: 2086, unfolded_bytes: 2086, executions: 19, zero_row_executions: 10, vm_steps: 222, fullscan_steps: 0, sorts: 7, autoindexes: 0, reprepares: 0, rows_returned: 4, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 1, written_term: 0 }\n",
        "c15 install Work { creates: 2477, schema_rows_parsed: 348026, prepared: 2571, prepared_bytes: 780500, unfolded_bytes: 780500, executions: 2571, zero_row_executions: 16, vm_steps: 73363, fullscan_steps: 40, sorts: 216, autoindexes: 0, reprepares: 0, rows_returned: 51, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 8, written_term: 38 }\n",
        "c15 empty settle Work { creates: 0, schema_rows_parsed: 0, prepared: 25, prepared_bytes: 3850, unfolded_bytes: 3850, executions: 25, zero_row_executions: 16, vm_steps: 422, fullscan_steps: 0, sorts: 9, autoindexes: 0, reprepares: 0, rows_returned: 4, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 1, written_term: 0 }",
    ));
}
