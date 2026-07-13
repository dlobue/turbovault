//! `manage_tags` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path → [`SinglePathOp`] mold; a mold-confirming sibling
//! of `edit_note` / `update_frontmatter` (current-API path: single-op
//! `batch_execute` of a `ManageTags` add).

use super::{Case, SinglePathOp};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;

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
        let hash = match &pc {
            Precondition::ExpectBlob(oid) => Some(oid.clone()),
            Precondition::ExpectExists => None,
            Precondition::Blind | Precondition::ExpectAbsent => {
                unreachable!("manage_tags only carries ExpectExists / ExpectBlob")
            }
        };
        let op = BatchOperation::ManageTags {
            path: rel.to_string(),
            operation: "add".to_string(),
            tags: vec![TAG.to_string()],
            expected_hash: hash,
        };
        let res = world.tools.batch_execute(vec![op]).await.map(|_| ());
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
/// `edit_note` / `update_frontmatter`. Same vehicle as update_frontmatter (MCP
/// tool blind-overwrites; this adapter drives the batch fold), so the same
/// pending split: only the two clean cells pass. Split set by running (§6).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    Case::pending(P::Exists, S::Absent, O::NoFile, CREATE_ON_ABSENT),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::pending(
        P::Exists,
        S::CommittedStaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::NewStaged, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(P::Exists, S::IntentToAdd, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(
        P::Exists,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError, DIRTY_GATE),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::pending(
        P::Head,
        S::CommittedStaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    Case::pending(P::Index, S::CommittedStaged, O::Ok, VS_HEAD),
    Case::pending(P::Index, S::NewStaged, O::Ok, VS_HEAD),
    Case::pending(
        P::Index,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        VS_HEAD,
    ),
    Case::pending(P::Index, S::NewStagedUnstaged, O::ConcurrencyError, VS_HEAD),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok, VS_HEAD),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok, VS_HEAD),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok, VS_HEAD),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok, VS_HEAD),
    Case::pending(P::Workdir, S::Untracked, O::Ok, VS_HEAD),
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent ──────────────
    Case::pending(P::Wrong, S::Absent, O::NoFile, CREATE_ON_ABSENT),
    Case::pending(P::Wrong, S::CleanCommitted, O::ConcurrencyError, VS_HEAD),
    Case::pending(P::Wrong, S::CommittedStaged, O::ConcurrencyError, VS_HEAD),
    Case::pending(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError, VS_HEAD),
    Case::pending(
        P::Wrong,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        VS_HEAD,
    ),
    Case::pending(P::Wrong, S::NewStaged, O::ConcurrencyError, VS_HEAD),
    Case::pending(P::Wrong, S::IntentToAdd, O::ConcurrencyError, VS_HEAD),
    Case::pending(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError, VS_HEAD),
    Case::pending(P::Wrong, S::Untracked, O::ConcurrencyError, VS_HEAD),
];

// Burndown reasons (same defects as update_frontmatter — no precondition-honoring
// write path today).
const CREATE_ON_ABSENT: &str =
    "GWS: in-place tag update on an absent target creates it; should NoFile";
const DIRTY_GATE: &str = "GWS: no dirty gate — commits uncommitted content on a dirty tree";
const HEAD_CLOBBER: &str = "GWS: HEAD token passes vs HEAD, clobbers the dirty tree";
const VS_HEAD: &str = "GWS: precondition checked vs HEAD, not the working tree (batch masks the refusal as a no-op Ok)";
