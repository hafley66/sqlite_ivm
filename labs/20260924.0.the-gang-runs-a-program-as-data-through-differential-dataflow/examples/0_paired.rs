//! Paired timing and RSS: `cargo run --release --example 0_paired [--features sqlite] -- <engine> <workload> <n>`.
//! One engine and one workload per process so RSS is not shared; prints one TSV row.

use lab_20260924_0::{Dd, Engine, Frontier, Program, SourceChange};
use std::time::{Duration, Instant};

fn program(name: &str) -> Program {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle").join(format!("{name}.program.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn change(rel: u32, row: Vec<i64>, w: i64) -> SourceChange {
    SourceChange { rel, row, w }
}

/// (program, load frontier, churn frontiers). Churn alternates insert and delete so state stays at load size.
fn workload(name: &str, n: i64) -> (Program, Frontier, Vec<Frontier>) {
    let teams = 100;
    match name {
        "access" => {
            let mut load: Vec<SourceChange> = (0..teams).map(|t| change(1, vec![t, 1000 + t], 1)).collect();
            load.extend((0..n).map(|p| change(0, vec![p, p % teams], 1)));
            let churn = (0..200).map(|i| change(0, vec![n + i / 2, (i / 2) % teams], if i % 2 == 0 { 1 } else { -1 }));
            (program("0_access"), Frontier { changes: load }, churn.map(|c| Frontier { changes: vec![c] }).collect())
        }
        "team_cost" => {
            let load = (0..n).map(|id| change(0, vec![id, id % teams, id % 17], 1)).collect();
            let churn = (0..200).map(|i| {
                let id = i / 2;
                let (from, to) = if i % 2 == 0 { (id % teams, (id + 1) % teams) } else { ((id + 1) % teams, id % teams) };
                Frontier { changes: vec![change(0, vec![id, from, id % 17], -1), change(0, vec![id, to, id % 17], 1)] }
            });
            (program("1_team_cost"), Frontier { changes: load }, churn.collect())
        }
        "team_sum" => {
            let load = (0..n).map(|id| change(0, vec![id, id % teams, id % 17], 1)).collect();
            let churn = (0..200).map(|i| {
                let id = i / 2;
                let (from, to) = if i % 2 == 0 { (id % teams, (id + 1) % teams) } else { ((id + 1) % teams, id % teams) };
                Frontier { changes: vec![change(0, vec![id, from, id % 17], -1), change(0, vec![id, to, id % 17], 1)] }
            });
            (program("10_team_sum"), Frontier { changes: load }, churn.collect())
        }
        "reach_tail" | "reach_middle" => {
            let load = (0..n).map(|x| change(0, vec![x, x + 1], 1)).collect();
            let edge = if name == "reach_tail" { vec![n, n + 1] } else { vec![n / 2, n / 2 + 1] };
            let first = if name == "reach_tail" { 1 } else { -1 };
            let churn = (0..20).map(|i| Frontier { changes: vec![change(0, edge.clone(), if i % 2 == 0 { first } else { -first })] });
            (program("7_reach"), Frontier { changes: load }, churn.collect())
        }
        other => panic!("unknown workload {other}"),
    }
}

fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

fn run<E: Engine>(engine: &str, name: &str, n: i64) {
    let (program, load, churn) = workload(name, n);
    let t0 = Instant::now();
    let mut e = E::install(&program, &mut lab_20260924_0::rel::Raw::default()).unwrap();
    let installed = t0.elapsed();
    let t1 = Instant::now();
    let loaded_rows = e.settle(load, &mut lab_20260924_0::rel::Raw::default()).unwrap().changes.len();
    let load_time = t1.elapsed();
    let mut times: Vec<Duration> = churn
        .into_iter()
        .map(|f| {
            let t = Instant::now();
            e.settle(f, &mut lab_20260924_0::rel::Raw::default()).unwrap();
            t.elapsed()
        })
        .collect();
    times.sort();
    let q = |p: usize| times[(times.len() - 1) * p / 100].as_micros();
    println!(
        "{engine}\t{name}\t{n}\tinstall_us={}\tload_ms={}\tload_out_rows={loaded_rows}\tchurn_p50_us={}\tchurn_p95_us={}\tchurn_max_us={}\trss_kib={}",
        installed.as_micros(),
        load_time.as_millis(),
        q(50),
        q(95),
        q(100),
        rss_kib()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (engine, name, n) = (args[0].as_str(), args[1].as_str(), args[2].parse().unwrap());
    match engine {
        "dd" => run::<Dd>(engine, name, n),
        #[cfg(feature = "sqlite")]
        "sqlite" => run::<lab_20260924_0::Sql>(engine, name, n),
        other => panic!("unknown engine {other}"),
    }
}
