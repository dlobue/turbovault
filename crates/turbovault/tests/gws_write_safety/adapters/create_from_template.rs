//! `create_from_template` adapter — a **create** op (design doc §4 default:
//! `ExpectAbsent`), structurally the same mold as `write_note`'s create path, so
//! it surfaces no new primitive requirement.
//!
//! Its matrix cells all need a *registered template* fixture in the `World`
//! (a `CreateFromTemplate` op with an unknown `template_id` just errors), which
//! is deferred to the full-grid phase. The adapter is scaffolded here — mold
//! identified, one-op invocation shape confirmed via `batch_execute` — with the
//! grid `#[ignore]`d pending that fixture.

use crate::harness::backend::World;
use crate::harness::state::{GitState, build_state};
use std::collections::HashMap;
use turbovault_tools::BatchOperation;

/// The single-op `batch_execute` shape a real grid would drive. Kept as a
/// reference so wiring the template fixture is the only remaining work.
#[allow(dead_code)]
async fn invoke(world: &World, path: &str, template_id: &str, hash_free: bool) -> bool {
    let _ = hash_free;
    let op = BatchOperation::CreateFromTemplate {
        template_id: template_id.to_string(),
        path: path.to_string(),
        fields: HashMap::new(),
        force: None,
    };
    world.tools.batch_execute(vec![op]).await.is_ok()
}

/// Placeholder: the create-mold grid mirrors `write_note`'s `ExpectAbsent`/
/// `Blind` cells but every OK cell needs a registered template. Un-ignore once
/// the `World` grows a template fixture.
#[tokio::test]
#[ignore = "GWS-nbl.2: create_from_template grid needs a registered-template fixture"]
async fn create_from_template_matrix() {
    let world = World::git();
    build_state(world.dir.path(), "note.md", GitState::Absent);
    // With a real template registered, this would be the ExpectAbsent create
    // path (OK on absent, ConcurrencyError on an existing/dirty target).
    let _created = invoke(&world, "note.md", "some-template", true).await;
}
