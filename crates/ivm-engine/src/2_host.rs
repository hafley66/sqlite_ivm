//! Frontier transport and output sink shared by raw callers and plugins.

use ivm_ir::{Delta, Frontier, Program};
use crate::{Engine, EngineError};
use std::collections::VecDeque;

pub trait Host {
    fn next(&mut self) -> Option<Frontier>;
    fn sink(&mut self, delta: &Delta) -> Result<(), EngineError>;
}

#[derive(Default)]
pub struct Raw {
    pub frontiers: VecDeque<Frontier>,
    pub deltas: Vec<Delta>,
}

impl Host for Raw {
    fn next(&mut self) -> Option<Frontier> {
        self.frontiers.pop_front()
    }

    fn sink(&mut self, delta: &Delta) -> Result<(), EngineError> {
        self.deltas.push(delta.clone());
        Ok(())
    }
}

/// The extension supplies its xSync collector and transactional result writer.
pub struct Plugin<'a> {
    pub collect: Box<dyn FnMut() -> Option<Frontier> + 'a>,
    pub write: Box<dyn FnMut(&Delta) -> Result<(), EngineError> + 'a>,
}

impl Host for Plugin<'_> {
    fn next(&mut self) -> Option<Frontier> {
        (self.collect)()
    }

    fn sink(&mut self, delta: &Delta) -> Result<(), EngineError> {
        (self.write)(delta)
    }
}

pub struct Runtime<E: Engine, H: Host> {
    pub engine: E,
    pub host: H,
}

impl<E: Engine, H: Host> Runtime<E, H> {
    pub fn install(program: &Program, host: H) -> Result<Self, EngineError> {
        Ok(Self { engine: E::install(program)?, host })
    }

    pub fn next(&mut self) -> Result<bool, EngineError> {
        let Some(frontier) = self.host.next() else {
            return Ok(false);
        };
        let delta = self.engine.settle(frontier)?;
        self.host.sink(&delta)?;
        Ok(true)
    }
}
