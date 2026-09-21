use lab_20260920_1::{
    median_leg, percents, profile_axes, ranked_upside, reuse_ceiling_pct, run_attributed,
    step_total, subscriber_delta_pct, LogStack, PROFILE_CHANGES, PROFILE_COLUMNS,
    PROFILE_JOIN_ARITY, PROFILE_SEED, PROFILE_TABLES,
};
use rig::rig::{Axes, PrepareMode, STEP_NAMES};

fn main() {
    println!(
        "profile: per single source-row change, tables={PROFILE_TABLES} columns={PROFILE_COLUMNS} join_arity={PROFILE_JOIN_ARITY} seed={PROFILE_SEED} changes={PROFILE_CHANGES}, median of legs"
    );
    let axes = profile_axes();

    let mut engine_on = Vec::new();
    let mut engine_off = Vec::new();
    let mut engine_trace = Vec::new();
    let mut cached_off = Vec::new();
    for _ in 0..3 {
        engine_on.push(run_attributed(&axes, PROFILE_CHANGES, PrepareMode::Engine, LogStack::Pinned));
        engine_off.push(run_attributed(&axes, PROFILE_CHANGES, PrepareMode::Engine, LogStack::Off));
        engine_trace.push(run_attributed(&axes, PROFILE_CHANGES, PrepareMode::Engine, LogStack::Trace));
        cached_off.push(run_attributed(&axes, PROFILE_CHANGES, PrepareMode::Cached, LogStack::Off));
    }
    let on = median_leg(engine_on);
    let off = median_leg(engine_off);
    let loud = median_leg(engine_trace);
    let cached = median_leg(cached_off);

    println!("\nnine steps, percent of step total (subscriber on, median leg):");
    println!("{:<22} {:>9} {:>16}", "step", "percent", "ns per change");
    let mut sql_side = 0.0f64;
    for (name, percent) in percents(&on) {
        let nanos = on
            .steps
            .iter()
            .find(|(step, _)| step == &name)
            .map(|(_, nanos)| *nanos)
            .unwrap_or_default();
        if name != "step/validate" && name != "step/build" {
            sql_side += percent;
        }
        println!("{:<22} {:>8.2}% {:>16}", name, percent, nanos / on.changes as u64);
    }
    println!(
        "split: SQL-side statements {:.2}% vs application-side validate+build {:.2}%",
        sql_side,
        100.0 - sql_side
    );
    println!(
        "step total covers {:.1}% of the fold wall ({:.1?})",
        100.0 * step_total(&on) as f64 / on.wall.as_nanos() as f64,
        on.wall
    );

    let delta = subscriber_delta_pct(&off, &on);
    println!(
        "\nsubscriber on vs off (same fold, same seed): off {:.1?} on {:.1?} delta {delta:+.2}%",
        off.wall, on.wall
    );

    let ceiling = reuse_ceiling_pct(&off, &cached);
    println!(
        "statement reuse ceiling: engine {:.1?} vs cached {:.1?} = {ceiling:.2}% of the fold",
        off.wall, cached.wall
    );
    let loud_delta = subscriber_delta_pct(&off, &loud);
    println!(
        "trace fallback, the state the pin prevents: {:.1?} ({loud_delta:+.2}% vs pinned off)",
        loud.wall
    );

    println!("\nlabs 3 through 9, ranked by measured upside:");
    for (rank, row) in ranked_upside(&on).iter().enumerate() {
        println!(
            "{}. lab {} {:>9.2}%  {}{}",
            rank + 1,
            row.lab,
            row.share_percent,
            row.knob,
            if row.note.is_empty() {
                String::new()
            } else {
                format!(" [{}]", row.note)
            }
        );
    }

    let narrow = Axes {
        columns: 10,
        ..axes
    };
    let m10 = run_attributed(&narrow, PROFILE_CHANGES, PrepareMode::Engine, LogStack::Off);
    println!("\nshape, per-change nanos M=10 vs M=20 (per-column steps grow, fixed steps hold):");
    for name in STEP_NAMES {
        let per_change = |attr: &lab_20260920_1::Attribution| {
            attr.steps
                .iter()
                .find(|(step, _)| step == &name)
                .map(|(_, nanos)| *nanos)
                .unwrap_or_default() as f64
                / attr.changes as f64
        };
        let (wide, thin) = (per_change(&off), per_change(&m10));
        println!("{:<22} {:>6.2}x", name, wide / thin);
    }
}
