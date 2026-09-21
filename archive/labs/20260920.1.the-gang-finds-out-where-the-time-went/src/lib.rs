use std::time::{Duration, Instant};

use rig::observe::{pin_log_filter, CountRecorder};
use rig::rig::{
    fold_plan, generate, install, run_fold, Axes, FoldReport, PrepareMode, STEP_NAMES,
};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

// The profile runs on the rig's canonical wide corner, unmodified.
pub const PROFILE_TABLES: usize = 8;
pub const PROFILE_COLUMNS: usize = 20;
pub const PROFILE_JOIN_ARITY: usize = 4;
pub const PROFILE_SEED: u64 = 42;
// One leg is one fold at this depth; well under the rig's fold budget so a
// leg stays far below the 10-second law even in a debug build.
pub const PROFILE_CHANGES: usize = 2048;
// Every leg runs three times and reports its median; a single pass proves
// nothing about a timing table.
pub const LEG_REPETITIONS: usize = 3;

pub fn profile_axes() -> Axes {
    Axes {
        tables: PROFILE_TABLES,
        columns: PROFILE_COLUMNS,
        join_arity: PROFILE_JOIN_ARITY,
        seed: PROFILE_SEED,
    }
}

// The rig's counting instrument is always installed; the subscriber toggle
// moves the log stack around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStack {
    // Counting layer only.
    Off,
    // Counting layer plus the filter and format layers hafley-observe::init
    // would install, gated by the pinned filter literal, never an ambient
    // variable.
    Pinned,
    // What the pin exists to prevent: the same stack with the filter at trace,
    // priced once so the pin's value is a measurement, not an article of
    // faith.
    Trace,
}

#[derive(Clone)]
pub struct Attribution {
    pub axes: Axes,
    pub changes: usize,
    pub mode: PrepareMode,
    pub log_stack: LogStack,
    pub wall: Duration,
    pub steps: Vec<(&'static str, u64)>,
}

// One fold with per-step wall attribution read off the rig's instrument. The
// log_stack argument only moves the logging subscriber around the always-on
// counting layer, so off still records timings.
pub fn run_attributed(
    axes: &Axes,
    changes: usize,
    mode: PrepareMode,
    log_stack: LogStack,
) -> Attribution {
    pin_log_filter();
    let tables = generate(axes).expect("generate");
    let db = rusqlite::Connection::open_in_memory().expect("open");
    install(&db, axes, &tables).expect("install");
    let (recorder, layer) = CountRecorder::new();
    let plan = fold_plan(axes, changes).expect("plan");
    let scope = match log_stack {
        LogStack::Off => Some(tracing::subscriber::set_default(
            tracing_subscriber::registry().with(layer),
        )),
        filter_level => {
            // EnvFilter added as a plain layer is a global filter and would
            // silence the instrument; attach it to the format layer only.
            let fmt = hafley_observe::format_layer(
                hafley_observe::FormatConfig::standard(hafley_observe::OutputFormat::Human, false),
                BoxMakeWriter::new(std::io::stderr),
            )
            .with_filter(EnvFilter::new(match filter_level {
                LogStack::Pinned => rig::observe::PINNED_LOG.to_string(),
                LogStack::Trace => "trace".to_string(),
                LogStack::Off => unreachable!(),
            }));
            Some(tracing::subscriber::set_default(
                tracing_subscriber::registry().with(layer).with(fmt),
            ))
        }
    };
    let started = Instant::now();
    let report: FoldReport = run_fold(&db, axes, &plan, mode).expect("fold");
    let wall = started.elapsed();
    drop(scope);
    let timings = recorder.timings();
    Attribution {
        axes: *axes,
        changes: report.changes,
        mode,
        log_stack,
        wall,
        steps: STEP_NAMES
            .iter()
            .map(|name| (*name, timings.nanos.get(*name).copied().unwrap_or_default()))
            .collect(),
    }
}

pub fn step_total(attr: &Attribution) -> u64 {
    attr.steps.iter().map(|(_, nanos)| nanos).sum::<u64>().max(1)
}

// Percent of the nine-step total per step. Ratios only: absolute timings are
// machine-specific and never belong in an assertion.
pub fn percents(attr: &Attribution) -> Vec<(&'static str, f64)> {
    let total = step_total(attr) as f64;
    attr.steps
        .iter()
        .map(|(name, nanos)| (*name, 100.0 * *nanos as f64 / total))
        .collect()
}

pub fn share(attr: &Attribution, names: &[&str]) -> f64 {
    let total = step_total(attr) as f64;
    let picked: u64 = attr
        .steps
        .iter()
        .filter(|(name, _)| names.contains(name))
        .map(|(_, nanos)| nanos)
        .sum();
    100.0 * picked as f64 / total
}

// (on - off) / off, in percent: the cost of watching the fold, measured, never
// assumed zero, because upstream defaults an unset filter to trace.
pub fn subscriber_delta_pct(off: &Attribution, on: &Attribution) -> f64 {
    let off_secs = off.wall.as_secs_f64();
    100.0 * (on.wall.as_secs_f64() - off_secs) / off_secs
}

// What reusing the three freshly prepared maintenance statements would save,
// as a fraction of the engine-mode fold. This is the lever that can make
// labs 3 through 9 moot.
pub fn reuse_ceiling_pct(engine: &Attribution, cached: &Attribution) -> f64 {
    let engine_secs = engine.wall.as_secs_f64();
    100.0 * (engine_secs - cached.wall.as_secs_f64()) / engine_secs
}

pub struct LabUpside {
    pub lab: &'static str,
    pub title: &'static str,
    pub knob: &'static str,
    pub share_percent: f64,
    pub note: &'static str,
}

// Which measured share each queued lab's knob would remove. Ties (6 and 7
// target the same plumbing) order by lab id, so the ranking itself is stable.
pub fn ranked_upside(attr: &Attribution) -> Vec<LabUpside> {
    let rows: [(&str, &str, &str, &[&str], &str); 7] = [
        (
            "3",
            "the-gang-asks-the-pragma-one-last-time",
            "hoist the pragma_recursive_triggers guard off the row clock",
            &["step/guard/pragma"],
            "",
        ),
        (
            "4",
            "the-gang-stops-talking-in-json",
            "hidden columns per source column instead of a JSON payload",
            &[],
            "already shipped: storage format 4 carries source rows in hidden __ivm_v columns, so the knob has nothing left to win",
        ),
        (
            "5",
            "the-gang-copies-the-fts5-homework",
            "buffer in xUpdate, flush in xSync",
            STEP_NAMES.as_slice(),
            "upper bound: the whole row-clock fold amortizes into one transaction flush",
        ),
        (
            "6",
            "the-gang-goes-through-the-side-door",
            "sqlite3_create_function instead of writing into the vtab",
            &["step/dispatch", "step/refresh", "step/validate", "step/build"],
            "",
        ),
        (
            "7",
            "the-gang-breaks-into-the-shadow-tables",
            "a scalar function writes shadow tables under DEFENSIVE",
            &["step/dispatch", "step/refresh", "step/validate", "step/build"],
            "same plumbing as 6, different mechanism",
        ),
        (
            "8",
            "the-gang-fires-all-the-triggers",
            "session extension replaces the trigger fleet",
            &["step/guard/pragma", "step/guard/types"],
            "",
        ),
        (
            "9",
            "the-gang-handles-the-whole-in-list",
            "set_in_constraint, read path only",
            &[],
            "read path; this write-path fold does not price it",
        ),
    ];
    let mut ranked: Vec<LabUpside> = rows
        .into_iter()
        .map(|(lab, title, knob, steps, note)| LabUpside {
            lab,
            title,
            knob,
            share_percent: share(attr, steps),
            note,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.share_percent
            .partial_cmp(&a.share_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.lab.cmp(b.lab))
    });
    ranked
}

// The leg whose wall is the median of the repetitions: the reported pass.
pub fn median_leg(mut legs: Vec<Attribution>) -> Attribution {
    assert!(!legs.is_empty(), "median of no legs");
    legs.sort_by_key(|leg| leg.wall);
    legs.remove(legs.len() / 2)
}
