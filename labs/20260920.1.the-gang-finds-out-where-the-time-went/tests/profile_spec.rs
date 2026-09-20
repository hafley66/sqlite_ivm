use lab_20260920_1::{
    percents, profile_axes, ranked_upside, run_attributed, subscriber_delta_pct, LogStack,
};
use rig::rig::{PrepareMode, STEP_GUARD_PRAGMA, STEP_GUARD_TYPES, STEP_NAMES, STEP_UPSERT, STEP_VALIDATE};

// The table is the lab's output: the nine per-step shares must account for the
// whole fold, and the measured steps must cover most of the wall so the
// attribution cannot be hiding a step twice or not at all.
#[test]
fn nine_steps_sum_to_total_wall_time() {
    let axes = profile_axes();
    let attr = run_attributed(&axes, 512, PrepareMode::Engine, LogStack::Pinned);
    let sum: f64 = percents(&attr).iter().map(|(_, percent)| percent).sum();
    assert!(
        (sum - 100.0).abs() < 0.5,
        "per-step percents sum to {sum}, not 100"
    );
    let coverage = attr.steps.iter().map(|(_, nanos)| nanos).sum::<u64>() as f64
        / attr.wall.as_nanos() as f64;
    assert!(
        coverage >= 0.5,
        "steps cover only {:.1}% of the fold wall",
        100.0 * coverage
    );
}

// The ranking is the artifact that schedules labs 3 through 9, so it must not
// move between runs on the same seed, and each step's share must hold steady.
#[test]
fn attribution_is_stable_across_runs() {
    let axes = profile_axes();
    let runs: Vec<_> = (0..3)
        .map(|_| run_attributed(&axes, 512, PrepareMode::Engine, LogStack::Off))
        .collect();
    for (index, run) in runs.iter().enumerate() {
        for (name, nanos) in &run.steps {
            let first = runs[0]
                .steps
                .iter()
                .find(|(step, _)| step == name)
                .map(|(_, nanos)| *nanos)
                .unwrap_or_default();
            let ratio = *nanos as f64 / first.max(1) as f64;
            assert!(
                (0.5..=2.0).contains(&ratio),
                "run {index} step {name} is {ratio:.2}x its run-0 time"
            );
        }
        let same_ranking = ranked_upside(run)
            .iter()
            .zip(ranked_upside(&runs[0]).iter())
            .all(|(a, b)| a.lab == b.lab);
        assert!(same_ranking, "lab ranking moved between runs");
    }
}

// The one number that can invalidate the whole profile, measured rather than
// assumed: what installing the subscriber stack costs the same fold.
#[test]
fn subscriber_overhead_is_measured_not_assumed() {
    let axes = profile_axes();
    let off = run_attributed(&axes, 512, PrepareMode::Engine, LogStack::Off);
    let on = run_attributed(&axes, 512, PrepareMode::Engine, LogStack::Pinned);
    let delta = subscriber_delta_pct(&off, &on);
    assert!(off.wall.as_nanos() > 0 && on.wall.as_nanos() > 0);
    // The instrument counts every span; if watching the fold ever multiplies
    // its cost like the trace fallback would, the profile is circular and this
    // suite must fail loudly.
    assert!(
        on.wall.as_secs_f64() / off.wall.as_secs_f64() < 4.0,
        "subscriber costs {delta:+.1}% of the fold"
    );
}

// Per-column steps must scale with M while the fixed ones hold, which is what
// separates the costs that rank the width-targeted labs from the rest.
#[test]
fn shape_holds_across_width() {
    let axes = profile_axes();
    let narrow = rig::rig::Axes {
        columns: 10,
        ..axes
    };
    let m10 = run_attributed(&narrow, 512, PrepareMode::Engine, LogStack::Off);
    let m20 = run_attributed(&axes, 512, PrepareMode::Engine, LogStack::Off);
    let per_change = |attr: &lab_20260920_1::Attribution, name: &str| {
        attr.steps
            .iter()
            .find(|(step, _)| *step == name)
            .map(|(_, nanos)| *nanos)
            .unwrap_or_default() as f64
            / attr.changes as f64
    };
    for name in STEP_NAMES {
        let ratio = per_change(&m20, name) / per_change(&m10, name).max(1.0);
        // Measured at 2048 changes: per-column steps run 1.29x..1.36x from
        // M=10 to M=20; fixed steps run 0.99x..1.17x. Dispatch binds and
        // executes a statement whose text scales with M, but the per-row cost
        // is fixed, so it rides with the fixed steps.
        let per_column =
            !matches!(name, "step/guard/pragma" | "step/refresh" | "step/validate" | "step/dispatch");
        if per_column {
            assert!(ratio >= 1.2, "{name} did not grow with M (ratio {ratio:.2})");
        } else {
            assert!(ratio <= 2.0, "{name} grew with M (ratio {ratio:.2})");
        }
    }
    // Sanity on the two ends of the spectrum the shape claim is about.
    let types_ratio = per_change(&m20, STEP_GUARD_TYPES) / per_change(&m10, STEP_GUARD_TYPES);
    assert!(types_ratio >= 1.2, "typeof guard ratio {types_ratio:.2}");
    let _ = (STEP_UPSERT, STEP_VALIDATE);
}
