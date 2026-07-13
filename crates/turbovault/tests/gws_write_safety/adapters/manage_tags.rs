//! `manage_tags` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path → [`SinglePathOp`] mold; a sibling of `edit_note` /
//! `update_frontmatter`.
//!
//! **SPEC-FIRST** (design doc §10 commit 1): drives the *aspirational* tool-layer
//! method `GitFileTools::manage_tags(path, operation, tags, precondition)` — which
//! **does not exist yet**. Same extraction as `update_frontmatter` (move the op to
//! the tool layer, honoring a precondition). Does not compile until it lands.

use super::{Case, SinglePathOp};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;

const TAG: &str = "gws-tag";

#[derive(Clone, Copy)]
pub struct ManageTags;

impl SinglePathOp for ManageTags {
    fn name(&self) -> &'static str {
        "manage_tags"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, world: &World, rel: &str, pc: Precondition) -> Observed {
        let tags = [TAG.to_string()];
        // Aspirational tool-layer method (does not exist yet — spec-first).
        let res = world.tools.manage_tags(rel, "add", &tags, pc).await;
        let after = world.read(rel);
        observe(res, after)
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(TAG))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: tag {TAG:?} not present: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** manage_tags matrix — same in-place shape and desired outcomes as
/// `edit_note` / `update_frontmatter`. All `Case::new`: spec-first asserts the
/// target contract; the implementation is shaped to meet it.
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
