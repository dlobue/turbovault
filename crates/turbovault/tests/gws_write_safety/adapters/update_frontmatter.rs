//! `update_frontmatter` adapter — an in-place op (design doc §4 default:
//! `ExpectExists`, dirty-gated). Single-path → [`SinglePathOp`] mold.
//!
//! No standalone `GitFileTools` method exists; the current-API path is a
//! single-op `batch_execute`. Structurally identical to `edit_note`, so it
//! surfaces no new primitive requirement — a mold-confirming sibling.

use super::{Case, SinglePathOp};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use std::collections::HashMap;
use turbovault_tools::BatchOperation;

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
        let hash = match &pc {
            Precondition::ExpectBlob(oid) => Some(oid.clone()),
            Precondition::ExpectExists => None,
            Precondition::Blind | Precondition::ExpectAbsent => {
                unreachable!("update_frontmatter only carries ExpectExists / ExpectBlob")
            }
        };
        let mut fm = HashMap::new();
        fm.insert(KEY.to_string(), serde_json::json!(true));
        let op = BatchOperation::UpdateFrontmatter {
            path: rel.to_string(),
            frontmatter: fm,
            merge: Some(true),
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
/// {Exists, Head, Index, Workdir, Wrong}; the desired outcomes are identical to
/// `edit_note`'s (same matrix rows). The *pending* split diverges: the current
/// vehicle is not `edit_file` but a compute-then-write path — the MCP tool blind-
/// overwrites (`WriteMode::Overwrite, None`), and this adapter drives the
/// `batch_execute` fold — so almost every non-clean cell is a burndown item.
/// Split set by running (design doc §6 empirical method).
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

// Burndown reasons. update_frontmatter has NO precondition-honoring write path
// today (MCP tool blind-overwrites; this adapter drives the batch fold whose
// expect_blob is checked vs HEAD and whose refusal the batch masks as a no-op
// Ok) — so only the two clean cells (Exists/Head on etc--) pass.
const CREATE_ON_ABSENT: &str =
    "GWS: in-place frontmatter update on an absent target creates it; should NoFile";
const DIRTY_GATE: &str = "GWS: no dirty gate — commits uncommitted content on a dirty tree";
const HEAD_CLOBBER: &str = "GWS: HEAD token passes vs HEAD, clobbers the dirty tree";
const VS_HEAD: &str = "GWS: precondition checked vs HEAD, not the working tree (batch masks the refusal as a no-op Ok)";
