//! Shared, executable reference cases for the DD and SQLite IVM tracks.
//!
//! The existing bench `Arm` is the current adapter: setup installs one graph,
//! apply commits one source batch and checks the output against the independent
//! fixture oracle, and teardown checks lifecycle. This file specifies the
//! weight algebra separately because the public Arm currently exposes a
//! checksum, not the signed changes at each internal edge.

#[path = "../src/arms/mod.rs"]
mod arms;
#[path = "../src/fixture.rs"]
mod fixture;
#[path = "../src/oracle.rs"]
mod oracle;

use anyhow::{Context, Result};
use arms::{Arm, Setup};
use std::collections::{BTreeMap, BTreeSet};

type Row = Vec<i64>;
type Weight = i64;
type ZSet = BTreeMap<Row, Weight>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowDiff {
    row: Row,
    weight: Weight,
}

fn weights(diffs: impl IntoIterator<Item = RowDiff>) -> ZSet {
    let mut out = ZSet::new();
    for RowDiff { row, weight } in diffs {
        *out.entry(row).or_default() += weight;
    }
    out.retain(|_, weight| *weight != 0);
    out
}

fn add(left: &ZSet, right: &ZSet) -> ZSet {
    weights(left.iter().chain(right).map(|(row, weight)| RowDiff {
        row: row.clone(),
        weight: *weight,
    }))
}

fn negate(input: &ZSet) -> ZSet {
    input
        .iter()
        .map(|(row, weight)| (row.clone(), -*weight))
        .collect()
}

fn distinct(input: &ZSet) -> ZSet {
    input
        .iter()
        .filter_map(|(row, weight)| {
            assert!(*weight >= 0, "settled support must be nonnegative");
            (*weight > 0).then(|| (row.clone(), 1))
        })
        .collect()
}

fn join(left: &ZSet, right: &ZSet) -> ZSet {
    weights(left.iter().flat_map(|(l, lw)| {
        right.iter().filter_map(move |(r, rw)| {
            (l[0] == r[0]).then(|| RowDiff {
                row: vec![l[0], l[1], r[1]],
                weight: lw * rw,
            })
        })
    }))
}

fn zset(rows: &[(&[i64], i64)]) -> ZSet {
    weights(rows.iter().map(|(row, weight)| RowDiff {
        row: row.to_vec(),
        weight: *weight,
    }))
}

fn delta(before: &ZSet, after: &ZSet) -> ZSet {
    add(after, &negate(before))
}

/// The same fixture and checkpoints go through each existing bench arm.
/// Each arm independently verifies source rows, output rows, and checksum.
fn assert_arm<A: Arm>(mut arm: A, family: &str, domain: oracle::Domain) -> Result<()> {
    let fixture = fixture::make_fixture(family, 8, 2, 2, domain)
        .with_context(|| format!("fixture {family}"))?;
    match arm.setup(&fixture)? {
        Setup::Ready => {}
        Setup::Unsupported { reason } => anyhow::bail!("{family}: {reason}"),
    }
    for state in &fixture.states {
        let measurement = arm
            .apply(state)
            .with_context(|| format!("{} checkpoint {}", arm.name(), state.name))?;
        assert_eq!(
            measurement.checksum, state.expected.checksum,
            "{} checkpoint {}",
            family, state.name
        );
    }
    arm.teardown()
        .with_context(|| format!("{} teardown {family}", arm.name()))
}

#[test]
fn concat_adds_weights_then_distinct_tracks_visibility() {
    let first = zset(&[(&[1], 2)]);
    let second = zset(&[(&[1], 1), (&[2], 1)]);
    assert_eq!(add(&first, &second), zset(&[(&[1], 3), (&[2], 1)]));
    assert_eq!(
        distinct(&add(&first, &second)),
        zset(&[(&[1], 1), (&[2], 1)])
    );
    assert_eq!(
        delta(&distinct(&first), &distinct(&add(&first, &second))),
        zset(&[(&[2], 1)])
    );
    assert_eq!(add(&first, &negate(&first)), ZSet::new());
}

#[test]
fn join_batch_includes_the_cross_term_once() {
    let old_left = zset(&[(&[1, 10], 1)]);
    let old_right = zset(&[(&[1, 20], 1)]);
    let left_delta = zset(&[(&[1, 11], 1)]);
    let right_delta = zset(&[(&[1, 21], 1)]);
    let actual = delta(
        &join(&old_left, &old_right),
        &join(&add(&old_left, &left_delta), &add(&old_right, &right_delta)),
    );
    let expanded = add(
        &add(
            &join(&left_delta, &old_right),
            &join(&old_left, &right_delta),
        ),
        &join(&left_delta, &right_delta),
    );
    assert_eq!(actual, expanded);
    assert_eq!(
        actual,
        zset(&[(&[1, 11, 20], 1), (&[1, 10, 21], 1), (&[1, 11, 21], 1),])
    );
}

#[test]
fn diamond_retraction_keeps_shared_reachability() {
    let roots = BTreeSet::from([1]);
    let before_edges = BTreeSet::from([(1, 2), (1, 3), (2, 4), (3, 4)]);
    let after_edges = BTreeSet::from([(1, 2), (1, 3), (3, 4)]);
    let reach = |edges: &BTreeSet<(i64, i64)>| {
        let mut reached = roots.clone();
        loop {
            let next = reached
                .iter()
                .copied()
                .chain(
                    edges
                        .iter()
                        .filter(|(from, _)| reached.contains(from))
                        .map(|(_, to)| *to),
                )
                .collect::<BTreeSet<_>>();
            if next == reached {
                break;
            }
            reached = next;
        }
        weights(reached.into_iter().map(|id| RowDiff {
            row: vec![id],
            weight: 1,
        }))
    };
    assert_eq!(
        delta(&reach(&before_edges), &reach(&after_edges)),
        ZSet::new()
    );
}

#[test]
fn dd_reference_matches_all_integer_fixtures() -> Result<()> {
    for family in fixture::CIRCUITS.into_iter().chain(fixture::SEMANTIC) {
        assert_arm(arms::dd::Dd::new(), family, oracle::Domain::Integers)?;
    }
    Ok(())
}

#[test]
fn sqlite_extension_matches_all_integer_fixtures() -> Result<()> {
    let extension = arms::sqlite_ivm::SqliteIvm::resolve_extension()?;
    assert!(
        extension.is_file(),
        "extension missing at {}",
        extension.display()
    );
    for family in fixture::CIRCUITS.into_iter().chain(fixture::SEMANTIC) {
        let db = std::env::temp_dir().join(format!(
            "sqlite-ivm-contract-{}-{family}.db",
            std::process::id(),
        ));
        assert_arm(
            arms::sqlite_ivm::SqliteIvm::new(db, extension.clone()),
            family,
            oracle::Domain::Integers,
        )?;
    }
    Ok(())
}
