//! Oni library crate.
//!
//! The rewrite keeps responsibilities split into small modules so transport,
//! planning, filesystem updates, and transfer strategies can evolve
//! independently.

pub mod chunker;
pub mod cli;
pub mod error;
pub mod staging;
pub mod manifest;
pub mod path;
pub mod plan;
pub mod protocol;
pub mod session;
pub mod strategy;
pub mod transport;
