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

const CASES: &[Case] = &[
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // FINDING: updating frontmatter of an absent file currently CREATES it;
    // an in-place op should NoFile instead.
    Case::pending(
        P::Exists,
        S::Absent,
        O::NoFile,
        "GWS: in-place frontmatter update on an absent target creates it; should NoFile",
    ),
    // FINDING: a wrong expected_hash is not enforced for the batch
    // UpdateFrontmatter fold — it should refuse (silent-overwrite CAS gap).
    Case::pending(
        P::Wrong,
        S::CleanCommitted,
        O::ConcurrencyError,
        "GWS/verify: batch UpdateFrontmatter does not enforce expected_hash",
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "GWS: no dirty gate for in-place frontmatter update",
    ),
];
