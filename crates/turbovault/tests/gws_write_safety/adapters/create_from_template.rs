//! `create_from_template` adapter — a **create** op (design doc §4 default:
//! `ExpectAbsent`), structurally the same mold as `write_note`'s create path, so
//! it surfaces no new primitive requirement.
//!
//! Its real matrix cells all need a *registered template* fixture in the `World`
//! (a `CreateFromTemplate` op with an unknown `template_id` just errors), which
//! is deferred. The adapter is scaffolded — mold identified, one-op invocation
//! shape confirmed via `batch_execute` — as a single **ignored** (pending) trial.

use libtest_mimic::Trial;
use std::collections::HashMap;

use super::cell_trial;
use crate::harness::backend::World;
use crate::harness::state::{GitState, build_state};
use turbovault_tools::BatchOperation;

pub fn trials() -> Vec<Trial> {
    vec![cell_trial(
        "create_from_template::grid::needs-template-fixture".to_string(),
        Some("GWS-nbl.2: create_from_template grid needs a registered-template fixture"),
        || async {
            let world = World::git();
            build_state(world.dir.path(), "note.md", GitState::Absent);
            // With a real template registered this is the ExpectAbsent create path
            // (OK on absent, ConcurrencyError on an existing/dirty target). Ignored
            // until the fixture lands; the body just confirms the invocation shape.
            let op = BatchOperation::CreateFromTemplate {
                template_id: "some-template".to_string(),
                path: "note.md".to_string(),
                fields: HashMap::new(),
                force: None,
            };
            let _ = world.tools.batch_execute(vec![op]).await;
            Ok(())
        },
    )]
}
