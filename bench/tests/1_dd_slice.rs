//! A real DD circuit hosted directly by redux::Slice.
//!
//! This proves the call boundary, not reducer purity: `Dd::apply` sends the
//! source batch to a Timely worker, advances all inputs, and waits on its probe
//! before this slice emits `Settled`. DD retains its own arrangements and queue.

#[path = "../src/arms/mod.rs"]
mod arms;
#[path = "../src/fixture.rs"]
mod fixture;
#[path = "../src/oracle.rs"]
mod oracle;

use anyhow::{bail, Result};
use arms::{Arm, Setup};
use fixture::{Fixture, State};
use redux::{reduce_then_apply, Slice};

struct DdState {
    circuit: arms::dd::Dd,
    next_epoch: u64,
    installed: bool,
}

impl Default for DdState {
    fn default() -> Self {
        Self {
            circuit: arms::dd::Dd::new(),
            next_epoch: 0,
            installed: false,
        }
    }
}

enum DdCall {
    Install(Fixture),
    RunBatch(State),
    Close,
}

#[derive(Debug, PartialEq, Eq)]
struct Settled {
    epoch: u64,
    checkpoint: String,
    checksum: String,
}

struct DdSlice;

impl Slice for DdSlice {
    type Context<'a> = ();
    type State = DdState;
    type Event = DdCall;
    type Output = Result<()>;
    type Effect = Settled;

    fn reduce(
        state: &mut Self::State,
        event: Self::Event,
        _: Self::Context<'_>,
        effect: &mut impl FnMut(Self::Effect),
    ) -> Self::Output {
        match event {
            DdCall::Install(fixture) => {
                if state.installed {
                    bail!("DD circuit already installed");
                }
                match state.circuit.setup(&fixture)? {
                    Setup::Ready => state.installed = true,
                    Setup::Unsupported { reason } => bail!("DD rejected circuit: {reason}"),
                }
            }
            DdCall::RunBatch(checkpoint) => {
                if !state.installed {
                    bail!("DD circuit missing");
                }
                let result = state.circuit.apply(&checkpoint)?;
                state.next_epoch += 1;
                effect(Settled {
                    epoch: state.next_epoch,
                    checkpoint: checkpoint.name,
                    checksum: result.checksum,
                });
            }
            DdCall::Close => {
                if state.installed {
                    state.circuit.teardown()?;
                    state.installed = false;
                }
            }
        }
        Ok(())
    }
}

#[test]
fn dd_slice_emits_only_after_join_and_recursive_batches_settle() -> Result<()> {
    for family in ["join", "reach_cycle"] {
        let fixture = fixture::make_fixture(family, 8, 2, 2, oracle::Domain::Integers)?;
        let mut state = DdState::default();
        let mut scratch = Vec::new();
        let mut published = Vec::new();
        reduce_then_apply::<DdSlice, _>(
            &mut state,
            DdCall::Install(fixture.clone()),
            (),
            &mut scratch,
            |_, item| published.push(item),
        )?;
        assert!(published.is_empty(), "installation published a batch");
        for (index, checkpoint) in fixture.states.into_iter().enumerate() {
            let expected = checkpoint.expected.checksum.clone();
            let name = checkpoint.name.clone();
            reduce_then_apply::<DdSlice, _>(
                &mut state,
                DdCall::RunBatch(checkpoint),
                (),
                &mut scratch,
                |_, item| published.push(item),
            )?;
            assert_eq!(
                published.last(),
                Some(&Settled {
                    epoch: index as u64 + 1,
                    checkpoint: name,
                    checksum: expected,
                })
            );
            assert_eq!(published.len(), index + 1);
        }
        reduce_then_apply::<DdSlice, _>(&mut state, DdCall::Close, (), &mut scratch, |_, item| {
            published.push(item)
        })?;
        assert!(!state.installed);
    }
    Ok(())
}
