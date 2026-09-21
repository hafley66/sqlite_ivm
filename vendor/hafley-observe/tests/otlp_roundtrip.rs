#![cfg(feature = "otlp-trace")]

//! End-to-end OTLP roundtrip: the `otlp_probe` example exports three spans to
//! a locally running otel-desktop-viewer, and the DuckDB file it writes is
//! queried back through the `duckdb` CLI.
//!
//! Skipped when otel-desktop-viewer is not found. The receiver is never faked:
//! the test either runs the real viewer or does nothing.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn executable(env_var: &str, name: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(env_var)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| on_path(name))
        .or_else(|| {
            let path = PathBuf::from(fallback);
            path.is_file().then_some(path)
        })
}

fn wait_for_listener(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn example_binary() -> PathBuf {
    let current = std::env::current_exe().expect("current exe");
    let debug = current
        .parent()
        .and_then(Path::parent)
        .expect("deps directory");
    let candidate = debug.join("examples").join("otlp_probe");
    if candidate.is_file() {
        candidate
    } else {
        debug.join("otlp_probe")
    }
}

#[test]
fn otlp_probe_spans_land_in_duckdb() {
    let Some(viewer) = executable("HAFLEY_OTLP_VIEWER", "otel-desktop-viewer", "") else {
        eprintln!("skipped: otel-desktop-viewer not on PATH");
        return;
    };
    let db = std::env::temp_dir().join(format!("observe-otlp-{}.duckdb", std::process::id()));
    let _ = std::fs::remove_file(&db);

    let mut child = Command::new(&viewer)
        .args([
            "--db",
            db.to_str().expect("db path"),
            "--open-browser=false",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn otel-desktop-viewer");
    assert!(
        wait_for_listener(4318, Duration::from_secs(20)),
        "otel-desktop-viewer did not listen on 4318"
    );

    let probe = Command::new(example_binary())
        .env("HAFLEY_OTLP_ENDPOINT", ENDPOINT)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run otlp_probe example");
    assert!(probe.success(), "otlp_probe exited with {probe}");

    // The DuckDB file stays locked while the viewer runs; stop it with SIGTERM
    // so the WAL is checkpointed before the query.
    let _ = Command::new("kill").arg(child.id().to_string()).status();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(500));

    let duckdb = executable("HAFLEY_DUCKDB", "duckdb", "/opt/homebrew/bin/duckdb")
        .expect("duckdb CLI not on PATH");
    let output = Command::new(duckdb)
        .arg("-readonly")
        .arg(&db)
        .arg("SELECT name FROM spans ORDER BY start_time")
        .output()
        .expect("query duckdb");
    assert!(
        output.status.success(),
        "duckdb query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8_lossy(&output.stdout);
    for name in ["probe", "parse", "lower"] {
        assert!(rows.contains(name), "span {name} missing from:\n{rows}");
    }

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("duckdb.wal"));
}
