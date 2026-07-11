//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection).
//!
//! This is the shape that drove the runner inversion: it targets *two* paths
//! with *two* states, so it can't ride the single-path mold. It builds its own
//! `Vec<CellOutcome>` and reports it exactly like the single-path ops — proving
//! the thin runner accommodates the odd shapes.
//!
//! THIN: just enough cells to lock the interface. The full dual-path matrix
//! (source 9-state × destination axis) is turbovault-9n6.

use crate::harness::adapter::CellOutcome;
use crate::harness::backend::{World, observe};
use crate::harness::outcome::Outcome::{self, ConcurrencyError, Ok as OkOutcome};
use crate::harness::runner::report;
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

async fn run() -> Vec<CellOutcome> {
    let mut out = Vec::new();
    for c in CASES {
        let world = World::git();
        build_state(world.dir.path(), FROM, c.from);
        build_state(world.dir.path(), TO, c.to);

        let before_to = world.read(TO);
        let res = world.tools.move_file(FROM, TO).await;
        let after_to = world.read(TO);
        let after_from = world.read(FROM);

        // The concurrency / no-clobber dimension is checked against the
        // destination's pre-move content.
        let observed = observe(res, after_to.clone());
        let mut result = c.expected.check(&observed, before_to.as_deref());
        // OK effect for a move: the source is gone and the destination now exists.
        if result.is_ok() && c.expected == OkOutcome {
            if after_from.is_some() {
                result = Err("OK effect: source still present after move".into());
            } else if after_to.is_none() {
                result = Err("OK effect: destination missing after move".into());
            }
        }

        out.push(CellOutcome {
            label: format!(
                "move_note / {} -> {} / expect {:?}",
                c.from.code(),
                c.to.code(),
                c.expected
            ),
            result,
            pending: c.pending,
        });
    }
    out
}

#[tokio::test]
async fn move_note_matrix() {
    report("move_note", run().await);
}
