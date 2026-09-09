#[path = "33a_dd_host.rs"]
mod host;
#[path = "37_semantic_graphs.rs"]
mod semantic_graphs;
use differential_dataflow::VecCollection;
fn graph<'s>(
    family: &str,
    a: VecCollection<'s, u64, [i64; 3]>,
    b: VecCollection<'s, u64, [i64; 3]>,
    _c: VecCollection<'s, u64, [i64; 3]>,
) -> VecCollection<'s, u64, Vec<i64>> {
    semantic_graphs::graph(family, a, b)
}
fn main() {
    host::run(graph);
}
