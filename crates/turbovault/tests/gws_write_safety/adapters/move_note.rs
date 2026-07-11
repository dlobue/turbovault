//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection).
//!
//! The shape that drove the runner inversion: two paths, two states, so it can't
//! ride the single-path mold. It builds its own trials via [`cell_trial`],
//! exactly like the single-path ops — proving the trial layer accommodates the
//! odd shapes. THIN: just the two cells that lock the shape; the full dual-path
//! matrix is turbovault-9n6.

use libtest_mimic::Trial;

use super::cell_trial;
use crate::harness::backend::{World, observe};
use crate::harness::outcome::Outcome::{self, ConcurrencyError, Ok as OkOutcome};
use crate::harness::state::GitState::{self, Absent, CleanCommitted};
use crate::harness::state::build_state;

const FROM: &str = "from.md";
const TO: &str = "to.md";

/// A dual-path cell: a source state × a destination state → the desired outcome.
struct MoveCase {
    from: GitState,
    to: GitState,
    expected: Outcome,
    pending: Option<&'static str>,
}

const CASES: &[MoveCase] = &[
    // Move onto an empty destination relocates the note.
    MoveCase {
        from: CleanCommitted,
        to: Absent,
        expected: OkOutcome,
        pending: None,
    },
    // Move onto an existing destination must refuse (dest ExpectAbsent = clobber
    // protection) rather than overwrite it.
    MoveCase {
        from: CleanCommitted,
        to: CleanCommitted,
        expected: ConcurrencyError,
        pending: None,
    },
];

pub fn trials() -> Vec<Trial> {
    CASES
        .iter()
        .map(|c| {
            let name = format!(
                "move_note::{}->{}::{:?}",
                c.from.code(),
                c.to.code(),
                c.expected
            );
            cell_trial(name, c.pending, move || run_move(c))
        })
        .collect()
}

async fn run_move(c: &MoveCase) -> Result<(), String> {
    let world = World::git();
    build_state(world.dir.path(), FROM, c.from);
    build_state(world.dir.path(), TO, c.to);

    let before_to = world.read(TO);
    let res = world.tools.move_file(FROM, TO).await;
    let after_to = world.read(TO);
    let after_from = world.read(FROM);

    // Concurrency / no-clobber dimension is checked against the destination's
    // pre-move content.
    c.expected
        .check(&observe(res, after_to.clone()), before_to.as_deref())?;
    // OK effect for a move: the source is gone and the destination now exists.
    if c.expected == OkOutcome {
        if after_from.is_some() {
            return Err("OK effect: source still present after move".into());
        }
        if after_to.is_none() {
            return Err("OK effect: destination missing after move".into());
        }
    }
    Ok(())
}
