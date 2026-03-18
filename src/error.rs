use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Cli(#[from] CliError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("unsupported invocation: {0}")]
    UnsupportedInvocation(String),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error("session state violation: {0}")]
    StateViolation(&'static str),
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport is not implemented: {feature}")]
    NotImplemented { feature: &'static str },
    #[error("transport closed unexpectedly")]
    Closed,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("planner is not implemented: {feature}")]
    NotImplemented { feature: &'static str },
    #[error("invalid relative path in plan: {path}")]
    InvalidRelativePath { path: PathBuf },
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("applier is not implemented: {feature}")]
    NotImplemented { feature: &'static str },
    #[error("refusing to escape destination root: {path}")]
    PathEscape { path: PathBuf },
}
