//! `edit_note` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path, so it rides the [`SinglePathOp`] mold.
//!
//! It surfaces one new primitive need: the SEARCH-not-found case is an
//! [`Outcome::OpError`] — a refusal that is neither a concurrency conflict nor a
//! missing file. That's an op-specific one-off (it varies the SEARCH text, not
//! the precondition/state), so it lives in its own test outside the grid.

use crate::harness::adapter::{Case, REL, SinglePathOp, run_single_path};
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::runner::report;
use crate::harness::state::{GitState as S, build_state};

/// The replacement text — `ok_effect` checks the edited file contains it.
const NEW: &str = "gws-edited";

pub struct EditNote;

/// A whole-content SEARCH/REPLACE block: SEARCH the file's current bytes,
/// replace with [`NEW`]. Matches whatever generation the state left on disk.
fn edits_replacing(current: &str) -> String {
    format!("<<<<<<< SEARCH\n{current}=======\n{NEW}\n>>>>>>> REPLACE\n")
}

impl SinglePathOp for EditNote {
    fn name(&self) -> &'static str {
        "edit_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, world: &World, rel: &str, pc: Precondition) -> Observed {
        let current = world.read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let expected_hash = match &pc {
            Precondition::ExpectBlob(oid) => Some(oid.clone()),
            // In-place default: it reads current, so it carries no caller token.
            Precondition::ExpectExists => None,
            Precondition::Blind | Precondition::ExpectAbsent => {
                unreachable!("edit_note only carries ExpectExists / ExpectBlob")
            }
        };
        let res = world
            .tools
            .edit_file(rel, &edits, expected_hash.as_deref(), false)
            .await
            .map(|_| ());
        let after = world.read(rel);
        observe(res, after)
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(NEW))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: expected edited content containing {NEW:?}, got {:?}",
                observed.after_content
            ))
        }
    }
}

/// Representative slice of the edit_note matrix (full grid follows). edit's
/// precondition axis is {Exists, Head, Index, Workdir, Wrong} — no Blind/Absent.
const CASES: &[Case] = &[
    // In-place default on a clean file edits it.
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // Edit requires an existing file.
    Case::new(P::Exists, S::Absent, O::NoFile),
    // A wrong token refuses.
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    // DEFECT: an in-place edit with no content proof on a dirty tree should
    // refuse — today it edits the dirty bytes and commits (no dirty gate).
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "GWS: no dirty gate for in-place edits",
    ),
];

#[tokio::test]
async fn edit_note_matrix() {
    report("edit_note", run_single_path(&EditNote).await);
}

/// One-off (outside the grid): a SEARCH that matches nothing is an `OpError` —
/// the op refuses, the working tree is untouched.
#[tokio::test]
async fn edit_note_search_not_found_is_op_error() {
    let world = World::git();
    build_state(world.dir.path(), REL, S::CleanCommitted);
    let before = world.read(REL);

    let edits = "<<<<<<< SEARCH\nNONEXISTENT-TEXT\n=======\nx\n>>>>>>> REPLACE\n";
    let res = world
        .tools
        .edit_file(REL, edits, None, false)
        .await
        .map(|_| ());
    let after = world.read(REL);

    let observed = observe(res, after);
    if let Err(e) = O::OpError.check(&observed, before.as_deref()) {
        panic!("edit_note SEARCH-not-found: {e}");
    }
}
