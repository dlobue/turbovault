//! Shared harness primitives for the GWS write-safety matrix (turbovault-nbl.1).
//!
//! Primitives here are backend-independent and know nothing about any operation
//! under test. Each mutating op is a thin adapter (later phases) that composes
//! these. See the design doc §7.

// Shared by two test binaries: the default-harness `gws_write_safety`
// (self-tests) and the `harness = false` `gws_matrix` (cells). Each exercises a
// different slice of the harness, and `gws_matrix` compiles the self-test `mod
// tests` without running them — so per-target "unused" is expected here.
#![allow(dead_code, unused_imports)]

pub mod backend;
pub mod outcome;
pub mod precondition;
pub mod state;
