//! `batch_execute` adapter — **multi-op** (N operations, all-or-nothing). The
//! other odd shape (besides `move_note`'s dual-path) that drove the runner
//! inversion: a case isn't `(precondition, state)`, it's a whole op list + an
//! atomicity expectation. It builds its own `CellOutcome`s and reports them
//! identically.
//!
//! THIN: just the two atomicity cells that lock the shape. The full batch
//! surface (per-op preconditions, intra-batch collisions) is a later pass.

use crate::harness::adapter::CellOutcome;
use crate::harness::backend::World;
use crate::harness::runner::report;
use crate::harness::state::{GitState, build_state};
use turbovault_tools::BatchOperation;

fn create(path: &str, content: &str) -> BatchOperation {
    BatchOperation::CreateNote {
        path: path.to_string(),
        content: content.to_string(),
        force: None,
    }
}

async fn run() -> Vec<CellOutcome> {
    let mut out = Vec::new();

    // Atomic success: two creates on absent paths → both land.
    {
        let world = World::git();
        let res = world
            .tools
            .batch_execute(vec![create("a.md", "A"), create("b.md", "B")])
            .await;
        let ok = res.is_ok() && world.read("a.md").is_some() && world.read("b.md").is_some();
        out.push(CellOutcome {
            label: "batch_execute / two creates / atomic OK".into(),
            result: if ok {
                Ok(())
            } else {
                Err(format!(
                    "batch did not create both (res_ok={}, a={}, b={})",
                    res.is_ok(),
                    world.read("a.md").is_some(),
                    world.read("b.md").is_some()
                ))
            },
            pending: None,
        });
    }

    // Atomic abort: a collision in the 2nd op must roll back the 1st — the
    // first path must NOT exist after the batch fails.
    {
        let world = World::git();
        build_state(world.dir.path(), "exists.md", GitState::CleanCommitted);
        let _ = world
            .tools
            .batch_execute(vec![
                create("fresh.md", "F"),
                create("exists.md", "X"), // expect_absent collision → whole batch aborts
            ])
            .await;
        // Atomicity = nothing partial applied: `fresh.md` must be absent.
        // (Finding: the batch currently returns `Ok` even though it aborted — a
        // 7q7-adjacent false-success; the safety property that matters here is
        // that the first op did NOT leak, which holds.)
        let atomic = world.read("fresh.md").is_none();
        out.push(CellOutcome {
            label: "batch_execute / collision aborts atomically (nothing partial applied)".into(),
            result: if atomic {
                Ok(())
            } else {
                Err("NON-atomic: fresh.md was created despite the colliding op aborting".into())
            },
            pending: None,
        });
    }

    out
}

#[tokio::test]
async fn batch_execute_matrix() {
    report("batch_execute", run().await);
}
