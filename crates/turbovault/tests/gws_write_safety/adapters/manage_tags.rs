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

const CASES: &[Case] = &[
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // FINDING: adding a tag to an absent file currently CREATES it; an in-place
    // op should NoFile instead.
    Case::pending(
        P::Exists,
        S::Absent,
        O::NoFile,
        "GWS: in-place tag update on an absent target creates it; should NoFile",
    ),
    // FINDING: a wrong expected_hash is not enforced for the batch ManageTags
    // fold — it should refuse (silent-overwrite CAS gap).
    Case::pending(
        P::Wrong,
        S::CleanCommitted,
        O::ConcurrencyError,
        "GWS/verify: batch ManageTags does not enforce expected_hash",
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "GWS: no dirty gate for in-place tag update",
    ),
];
