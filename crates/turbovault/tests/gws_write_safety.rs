//! GWS write-safety matrix suite (aspirational).
//!
//! Backend-parameterized, matrix-driven tests asserting the *desired*
//! write-safety behavior of every mutating TurboVault operation across the
//! full space of working-tree state × precondition. Tests assert desired
//! behavior, so many fail (or are `#[ignore]`d) until the behavior lands.
//!
//! Design + full model: vault `projects/turbovault/gws-write-safety-test-suite-design.md`.
//! Authoritative per-cell outcomes: `gws-test-matrix - Matrix.csv` (repo root).
//!
//! This turn: Phase 0 — the reusable harness primitives (turbovault-nbl.1).

// An integration-test file is its own crate root, so `mod harness;` would look
// for a sibling `tests/harness.rs` (which cargo would compile as a stray test
// binary). `#[path]` keeps the harness in its own subtree; nested `mod`s inside
// harness.rs then resolve relative to `gws_write_safety/harness/`.
#[path = "gws_write_safety/harness/mod.rs"]
mod harness;

#[path = "gws_write_safety/adapters/mod.rs"]
mod adapters;
