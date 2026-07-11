//! `delete_note` adapter — an in-place op whose OK *effect* is that the target
//! is **gone** (not that content changed). Single-path → [`SinglePathOp`] mold,
//! overriding `ok_effect`.
//!
//! The oz6 backlink axis (refuse to delete a note with inbound links) is a
//! tool-layer behavior today, not substrate-layer, so at this layer it's a
//! deferred one-off (noted below), tracked with the substrate move of oz6.

use crate::harness::adapter::{Case, SinglePathOp, run_single_path};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::runner::report;
use crate::harness::state::GitState as S;

pub struct DeleteNote;

impl SinglePathOp for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, world: &World, rel: &str, pc: Precondition) -> Observed {
        let hash = match &pc {
            Precondition::ExpectBlob(oid) => Some(oid.clone()),
            Precondition::ExpectExists => None,
            Precondition::Blind | Precondition::ExpectAbsent => {
                unreachable!("delete_note only carries ExpectExists / ExpectBlob")
            }
        };
        let res = world
            .tools
            .delete_file_with_hash(rel, hash.as_deref())
            .await;
        let after = world.read(rel);
        observe(res, after)
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed.after_content.is_none() {
            Ok(())
        } else {
            Err(format!(
                "OK effect: target still present after delete: {:?}",
                observed.after_content
            ))
        }
    }
}

/// Representative slice. delete's precondition axis is {Exists, Head, Index,
/// Workdir, Wrong} — no Blind/Absent (delete needs an existing target).
const CASES: &[Case] = &[
    // In-place default on a clean file deletes it.
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // Deleting an absent target is an idempotent no-op success (the goal —
    // absence — already holds). Distinct from edit, which needs content.
    Case::new(P::Exists, S::Absent, O::Ok),
    // A wrong token refuses.
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    // DEFECT: deleting with no content proof on a dirty tree should refuse —
    // today it deletes, discarding the uncommitted content.
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "GWS: no dirty gate for delete",
    ),
];

#[tokio::test]
async fn delete_note_matrix() {
    report("delete_note", run_single_path(&DeleteNote).await);
}
