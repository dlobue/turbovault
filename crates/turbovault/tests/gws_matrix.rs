//! GWS write-safety matrix — per-cell named runner (turbovault-nbl.2).
//!
//! `harness = false`: each matrix cell is its own `libtest-mimic` trial, so it
//! shows as a named `cargo test` entry — filterable (`cargo test --test
//! gws_matrix write_note::WORKDIR`), individually runnable, and counted one by
//! one in the output. `pending` cells are `ignored` trials (the burndown): run
//! them with `--ignored`; a pass there is the signal to un-pend.
//!
//! The harness's own unit tests (the meta tests) live in the default-harness
//! `gws_write_safety` target. Design: vault
//! `projects/turbovault/gws-write-safety-test-suite-design.md`.

#[path = "gws_write_safety/harness/mod.rs"]
mod harness;

#[path = "gws_write_safety/adapters/mod.rs"]
mod adapters;

use adapters::single_path_trials;
use libtest_mimic::{Arguments, Trial};

fn main() {
    let args = Arguments::from_args();

    let mut tests: Vec<Trial> = Vec::new();
    // Single-path ops (the SinglePathOp mold generates their full grids).
    tests.extend(single_path_trials(adapters::write_note::WriteNote));
    tests.extend(single_path_trials(adapters::edit_note::EditNote));
    tests.extend(single_path_trials(adapters::delete_note::DeleteNote));
    tests.extend(single_path_trials(
        adapters::update_frontmatter::UpdateFrontmatter,
    ));
    tests.extend(single_path_trials(adapters::manage_tags::ManageTags));
    // Op-specific one-offs + the odd shapes (dual-path, multi-op).
    tests.extend(adapters::edit_note::extra_trials());
    tests.extend(adapters::move_note::trials());
    tests.extend(adapters::batch_execute::trials());
    tests.extend(adapters::create_from_template::trials());

    libtest_mimic::run(&args, tests).exit();
}
