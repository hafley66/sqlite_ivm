//! `sqlite-ivm-bench`: shootout, scale sweep, and fixture dumps.
//!
//! Exit codes: 0 all checksums matched; 1 a fixture/oracle mismatch; 2 an
//! execution failure. No shell is spawned; external programs are limited to
//! initdb, pg_ctl, and gnuplot.

mod arms;
mod fixture;
mod oracle;
mod report;
mod scale;
mod scale_dd;
use std::path::{Path, PathBuf};
use anyhow::{anyhow, bail, Result};
use oracle::Domain;

fn main() {
    tracing_subscriber::fmt::init();
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            tracing::error!("{error:#}");
            2
        }
    };
    std::process::exit(code as i32);
}

fn run() -> Result<u8> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("shootout") => shootout(&args[1..]),
        Some("scale") => scale(&args[1..]),
        Some("dump-fixture") => dump_fixture(&args[1..]),
        _ => bail!(
            "usage: bench shootout [smoke|quick] [--engines e1,e2] [--out DIR] \
             [--circuits c1,c2] [--pg-prefix DIR]\n       \
             bench scale [--circuits c1,c2] [--n 10,100,...] [--fanout 1,10] [--arms a1,a2] [--reps N] [--out DIR]\n       \
             bench dump-fixture <circuit|all> [--rows N] [--batch N] [--fanout N] [--domain D]"
        ),
    }
}

type Flags = (Vec<String>, Vec<(String, String)>);

fn parse_flags(args: &[String]) -> Result<Flags> {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(key) = arg.strip_prefix("--") {
            let Some(value) = args.get(index + 1) else {
                bail!("flag --{key} needs a value");
            };
            flags.push((key.to_string(), value.clone()));
            index += 2;
        } else {
            positional.push(arg.clone());
            index += 1;
        }
    }
    Ok((positional, flags))
}

fn flag<'a>(flags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn list(value: &str) -> Vec<String> {
    value.split(',').map(|item| item.trim().to_string()).filter(|item| !item.is_empty()).collect()
}

fn default_out(kind: &str) -> PathBuf {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    std::path::PathBuf::from(format!("results/{kind}-{timestamp}-{}", std::process::id()))
}

fn fresh_out(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("output directory {} already exists", path.display());
    }
    Ok(())
}

fn shootout(args: &[String]) -> Result<u8> {
    let (positional, flags) = parse_flags(args)?;
    let smoke = match positional.first().map(String::as_str) {
        None => false,
        Some("quick") => false,
        Some("smoke") => true,
        Some(other) => bail!("unknown shootout profile {other}"),
    };
    let engines = match flag(&flags, "engines") {
        Some(requested) => {
            let requested = list(requested);
            for engine in &requested {
                if !report::DEFAULT_ENGINES.contains(&engine.as_str()) {
                    bail!("unknown engine {engine}");
                }
            }
            requested
        }
        None => report::DEFAULT_ENGINES.iter().map(|engine| engine.to_string()).collect(),
    };
    let circuits = flag(&flags, "circuits").map(list);
    let out = flag(&flags, "out").map(PathBuf::from).unwrap_or_else(|| default_out("shootout"));
    fresh_out(&out)?;
    let runner = report::Shootout {
        smoke,
        engines,
        out,
        circuits,
        pg_prefix: flag(&flags, "pg-prefix").map(PathBuf::from),
    };
    runner.run()
}

fn parse_i64s(items: Vec<String>) -> Result<Vec<i64>> {
    items
        .iter()
        .map(|item| item.parse::<i64>().map_err(|_| anyhow!("bad number {item}")))
        .collect()
}

 fn scale(args: &[String]) -> Result<u8> {
    let (_positional, flags) = parse_flags(args)?;
    let out = flag(&flags, "out").map(PathBuf::from).unwrap_or_else(|| default_out("scale"));
    fresh_out(&out)?;
    let arms = match flag(&flags, "arms") {
        Some(requested) => {
            let requested = list(requested);
            for arm in &requested {
                if !scale::ARMS.contains(&arm.as_str()) {
                    bail!("unknown arm {arm}; expected one of {:?}", scale::ARMS);
                }
            }
            requested
        }
        None => scale::ARMS.iter().map(|arm| arm.to_string()).collect(),
    };
    let reps: usize = flag(&flags, "reps").map(|v| v.parse()).transpose()?.unwrap_or(1);
    if reps == 0 {
        bail!("--reps must be at least 1");
    }
    let runner = scale::Scale {
        circuits: flag(&flags, "circuits").map(list).unwrap_or_else(|| {
            ["chain", "join", "group", "distinct", "window", "reach"]
                .map(String::from)
                .to_vec()
        }),
        ns: flag(&flags, "n")
            .map(list)
            .map(parse_i64s)
            .transpose()?
            .unwrap_or_else(|| vec![10, 100, 1000, 10000, 100000]),
        fanouts: flag(&flags, "fanout")
            .map(list)
            .map(parse_i64s)
            .transpose()?
            .unwrap_or_else(|| vec![1, 10]),
        arms,
        reps,
        out,
    };
    runner.run()
}

fn dump_fixture(args: &[String]) -> Result<u8> {
    let (positional, flags) = parse_flags(args)?;
    let Some(target) = positional.first() else {
        bail!("dump-fixture needs a circuit name or 'all'");
    };
    let rows: i64 = flag(&flags, "rows").map(|v| v.parse()).transpose()?.unwrap_or(400);
    let batch: i64 = flag(&flags, "batch").map(|v| v.parse()).transpose()?.unwrap_or(rows / 40);
    let fanout: i64 = flag(&flags, "fanout").map(|v| v.parse()).transpose()?.unwrap_or(10);
    let domain = Domain::parse(flag(&flags, "domain").unwrap_or("integers")).ok_or_else(
        || anyhow!("unknown value domain; expected integers|text_nocase|mixed_int_real"),
    )?;
    let names: Vec<&str> = if target == "all" {
        fixture::circuit_names()
    } else if fixture::circuit_names().contains(&&**target) {
        vec![target]
    } else {
        bail!("unknown circuit {target}")
    };
    let mut stdout = std::io::stdout().lock();
    for name in names {
        let fixture = fixture::make_fixture(name, rows, batch, fanout, domain)?;
        use std::io::Write;
        writeln!(stdout, "{}", serde_json::to_string_pretty(&fixture)?)?;
    }
    Ok(0)
}
