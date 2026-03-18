use std::path::{Path, PathBuf};

use crate::chunking::Chunker;
use crate::engine::apply::Applier;
use crate::engine::planner::Planner;
use crate::error::SessionError;
use crate::transport::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Local,
    RemotePush,
    RemotePull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Initialized,
    Scanning,
    Planning,
    Transferring,
    Applying,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionBudget {
    pub scan_queue_capacity: usize,
    pub transfer_queue_capacity: usize,
    pub apply_queue_capacity: usize,
    pub chunk_workers: usize,
}

impl Default for SessionBudget {
    fn default() -> Self {
        Self {
            scan_queue_capacity: 128,
            transfer_queue_capacity: 64,
            apply_queue_capacity: 32,
            chunk_workers: std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub mode: SessionMode,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub budget: SessionBudget,
}

pub struct Session<'a> {
    state: SessionState,
    pub config: SessionConfig,
    pub transport: &'a dyn Transport,
    pub planner: &'a dyn Planner,
    pub applier: &'a dyn Applier,
    pub chunker: &'a dyn Chunker,
}

impl<'a> Session<'a> {
    pub fn new(
        config: SessionConfig,
        transport: &'a dyn Transport,
        planner: &'a dyn Planner,
        applier: &'a dyn Applier,
        chunker: &'a dyn Chunker,
    ) -> Self {
        Self {
            state: SessionState::Initialized,
            config,
            transport,
            planner,
            applier,
            chunker,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn destination_root(&self) -> &Path {
        &self.config.destination
    }

    pub async fn run(&mut self) -> Result<(), SessionError> {
        self.state = SessionState::Scanning;
        self.state = SessionState::Planning;
        self.state = SessionState::Transferring;
        self.transport.handshake().await?;
        self.state = SessionState::Applying;
        self.state = SessionState::Completed;
        Ok(())
    }
}
