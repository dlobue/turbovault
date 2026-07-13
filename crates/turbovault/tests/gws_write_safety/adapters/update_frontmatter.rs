//! `update_frontmatter` adapter — an in-place op (design doc §4 default:
//! `ExpectExists`, dirty-gated). Single-path → [`SinglePathOp`] mold.
//!
//! **SPEC-FIRST** (design doc §10 commit 1): this drives the *aspirational*
//! tool-layer method `GitFileTools::update_frontmatter(path, frontmatter, merge,
//! precondition)` — which **does not exist yet**. Today the op has no
//! precondition-honoring tool-layer path (the MCP tool blind-overwrites; the only
//! substrate route is the `batch_execute` fold). Moving it to the tool layer,
//! honoring the precondition, is the extraction this test shapes. Until that
//! lands the `gws_matrix` binary does not build — the honest spec-first artifact.

use super::{Case, SinglePathOp};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use std::collections::HashMap;

const KEY: &str = "gws_touched";

#[derive(Clone, Copy)]
pub struct UpdateFrontmatter;

impl SinglePathOp for UpdateFrontmatter {
    fn name(&self) -> &'static str {
        "update_frontmatter"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, world: &World, rel: &str, pc: Precondition) -> Observed {
        let mut fm = HashMap::new();
        fm.insert(KEY.to_string(), serde_json::json!(true));
        // Aspirational tool-layer method (does not exist yet — spec-first).
        let res = world
            .tools
            .update_frontmatter(rel, &fm, Some(true), pc)
            .await;
        let after = world.read(rel);
        observe(res, after)
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(KEY))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: frontmatter key {KEY:?} not present: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** update_frontmatter matrix. In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong}; desired outcomes are identical to
/// `edit_note`'s (same matrix rows). Every cell is `Case::new` (active): spec-
/// first asserts the target contract and the implementation is shaped to meet it.
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    Case::new(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    Case::new(P::Index, S::CommittedStaged, O::Ok),
    Case::new(P::Index, S::NewStaged, O::Ok),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::new(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::new(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::new(P::Workdir, S::IntentToAdd, O::Ok),
    Case::new(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::new(P::Workdir, S::Untracked, O::Ok),
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent ──────────────
    Case::new(P::Wrong, S::Absent, O::NoFile),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];
