//! Shared harness primitives for the GWS write-safety matrix (turbovault-nbl.1).
//!
//! Primitives here are backend-independent and know nothing about any operation
//! under test. Each mutating op is a thin adapter (later phases) that composes
//! these. See the design doc §7.

pub mod backend;
pub mod outcome;
pub mod precondition;
pub mod state;
