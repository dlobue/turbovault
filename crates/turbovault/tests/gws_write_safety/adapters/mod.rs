//! Per-operation adapters (design doc §7). Each owns how to invoke its op with a
//! resolved precondition + its literal case table. Adding an op = one module.

pub mod batch_execute;
pub mod create_from_template;
pub mod delete_note;
pub mod edit_note;
pub mod manage_tags;
pub mod move_note;
pub mod update_frontmatter;
pub mod write_note;
