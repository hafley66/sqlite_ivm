//! Frontier transport and output sink shared by raw callers and plugins.

use ivm_ir::{Delta, Frontier, Program};
use crate::{Engine, EngineError};
use std::any::Any;
use std::collections::VecDeque;

pub trait Host {
    fn conn(&mut self) -> Option<&dyn Any>;
    fn next(&mut self) -> Option<Frontier>;
    fn sink(&mut self, delta: &Delta) -> Result<(), EngineError>;
}

pub struct Raw<'a> {
    pub connection: Option<&'a dyn Any>,
    pub frontiers: VecDeque<Frontier>,
    pub deltas: Vec<Delta>,
}

impl Default for Raw<'_> {
    fn default() -> Self {
        Self { connection: None, frontiers: VecDeque::new(), deltas: Vec::new() }
    }
}

impl<'a> Raw<'a> {
    pub fn with_connection(connection: &'a impl Any) -> Self {
        Self { connection: Some(connection), ..Self::default() }
    }
}

impl Host for Raw<'_> {
    fn conn(&mut self) -> Option<&dyn Any> {
        self.connection
    }

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
    pub connection: &'a dyn Any,
    pub collect: Box<dyn FnMut() -> Option<Frontier> + 'a>,
    pub write: Box<dyn FnMut(&Delta) -> Result<(), EngineError> + 'a>,
}

impl Host for Plugin<'_> {
    fn conn(&mut self) -> Option<&dyn Any> {
        Some(self.connection)
    }

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
    pub fn install(program: &Program, mut host: H) -> Result<Self, EngineError> {
        let engine = E::install(program, &mut host)?;
        Ok(Self { engine, host })
    }

    pub fn next(&mut self) -> Result<bool, EngineError> {
        let Some(frontier) = self.host.next() else {
            return Ok(false);
        };
        let delta = self.engine.settle(frontier, &mut self.host)?;
        self.host.sink(&delta)?;
        Ok(true)
    }
}
