//! `batch_execute` adapter — **multi-op** (N operations, all-or-nothing). The
//! other odd shape (besides `move_note`'s dual-path): a case is a whole op list
//! plus an atomicity expectation, not `(precondition, state)`. It builds its own
//! trials via [`cell_trial`]. THIN: the two atomicity cells that lock the shape.

use libtest_mimic::Trial;

use super::cell_trial;
use crate::harness::backend::World;
use crate::harness::state::{GitState, build_state};
use turbovault_tools::BatchOperation;

fn create(path: &str, content: &str) -> BatchOperation {
    BatchOperation::CreateNote {
        path: path.to_string(),
        content: content.to_string(),
        force: None,
    }
}

pub fn trials() -> Vec<Trial> {
    vec![
        // Atomic success: two creates on absent paths → both land.
        cell_trial(
            "batch_execute::two-creates::atomic-OK".to_string(),
            None,
            || async {
                let world = World::git();
                let res = world
                    .tools
                    .batch_execute(vec![create("a.md", "A"), create("b.md", "B")])
                    .await;
                if res.is_ok() && world.read("a.md").is_some() && world.read("b.md").is_some() {
                    Ok(())
                } else {
                    Err(format!(
                        "batch did not create both (res_ok={}, a={}, b={})",
                        res.is_ok(),
                        world.read("a.md").is_some(),
                        world.read("b.md").is_some()
                    ))
                }
            },
        ),
        // Atomic abort: a collision in the 2nd op must roll back the 1st — the
        // first path must NOT exist after the batch fails. (Finding: the batch
        // currently returns Ok even though it aborted — a 7q7-adjacent
        // false-success; the safety property that matters is atomicity.)
        cell_trial(
            "batch_execute::collision-aborts-atomically".to_string(),
            None,
            || async {
                let world = World::git();
                build_state(world.dir.path(), "exists.md", GitState::CleanCommitted);
                let _ = world
                    .tools
                    .batch_execute(vec![create("fresh.md", "F"), create("exists.md", "X")])
                    .await;
                if world.read("fresh.md").is_none() {
                    Ok(())
                } else {
                    Err("NON-atomic: fresh.md was created despite the colliding op aborting".into())
                }
            },
        ),
    ]
}
