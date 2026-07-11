//! Matrix-cell types + the single-path adapter mold (design doc §7).
//!
//! The runner is deliberately thin: each adapter owns its per-case execution and
//! hands back [`CellOutcome`]s that [`super::runner::report`] aggregates. That's
//! what lets the odd shapes fit without the runner special-casing them — a
//! dual-path (`move_note`) or multi-op (`batch_execute`) adapter builds its own
//! `Vec<CellOutcome>` however its shape requires, and reports it identically.
//!
//! Single-target ops (write / edit / delete / frontmatter / tags / template)
//! share the [`SinglePathOp`] mold + [`run_single_path`]; the odd shapes are
//! bespoke.

use super::backend::World;
use super::outcome::{Observed, Outcome};
use super::precondition::{Precondition, PreconditionKind};
use super::state::{GitState, build_state};

/// The result of exercising one matrix cell.
pub struct CellOutcome {
    /// Self-describing identity, e.g. `write_note / WORKDIR / etc-u / expect Ok`.
    pub label: String,
    /// `Ok(())` if the cell matched its expectation, `Err(reason)` on mismatch.
    pub result: Result<(), String>,
    /// `Some(reason)` if this is a not-yet-implemented burndown cell.
    pub pending: Option<&'static str>,
}

/// One single-path matrix cell: a precondition selector × a working-tree state →
/// the desired outcome. `pending` marks a cell whose desired behavior isn't
/// implemented yet.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub precondition: PreconditionKind,
    pub state: GitState,
    pub expected: Outcome,
    /// `Some(reason)` when the desired outcome isn't implemented yet: the cell is
    /// still exercised, but a mismatch is counted as *pending*, not a hard
    /// failure (the burndown). Flip to `None` when the behavior lands; the runner
    /// loudly flags a pending cell that has started passing.
    pub pending: Option<&'static str>,
}

impl Case {
    /// An active cell (its desired behavior must already hold).
    pub const fn new(precondition: PreconditionKind, state: GitState, expected: Outcome) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: None,
        }
    }

    /// A cell whose desired behavior is not yet implemented (burndown item).
    pub const fn pending(
        precondition: PreconditionKind,
        state: GitState,
        expected: Outcome,
        reason: &'static str,
    ) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: Some(reason),
        }
    }
}

/// The relative path every single-path op targets.
pub const REL: &str = "note.md";

/// A single-target operation: it targets exactly one path, so the runner can
/// build the state, resolve the precondition, snapshot, invoke, and check
/// generically. Generic dispatch only (native `async fn` in trait).
pub trait SinglePathOp {
    fn name(&self) -> &'static str;

    fn cases(&self) -> &'static [Case];

    /// Perform the op against `world` on [`REL`] with `precondition`, returning
    /// the normalized observation. Today maps [`Precondition`] onto the current
    /// `force`/`expected_hash` API; the P5 cutover swaps this body to the real
    /// precondition op — nothing else changes.
    async fn invoke(&self, world: &World, rel: &str, precondition: Precondition) -> Observed;

    /// OK-effect check, run only when a cell expects [`Outcome::Ok`]: assert the
    /// op's *specific* successful effect (content == X, target deleted, …).
    /// Default: a successful op is enough. Override per op.
    fn ok_effect(&self, _observed: &Observed) -> Result<(), String> {
        Ok(())
    }
}

/// Drive every case of a single-path op: build the state, resolve the
/// precondition (skip N/A), snapshot, invoke, check the outcome (+ the op's OK
/// effect). Returns one [`CellOutcome`] per constructible cell.
pub async fn run_single_path<Op: SinglePathOp>(op: &Op) -> Vec<CellOutcome> {
    let name = op.name();
    let mut out = Vec::new();
    for case in op.cases() {
        let world = World::git();
        let oids = build_state(world.dir.path(), REL, case.state);
        let Some(pc) = case.precondition.resolve(&oids) else {
            continue; // token undefined for this state — the matrix's N/A (omitted)
        };
        let before = world.read(REL);
        let observed = op.invoke(&world, REL, pc).await;

        let mut result = case.expected.check(&observed, before.as_deref());
        if result.is_ok() && case.expected == Outcome::Ok {
            result = op.ok_effect(&observed);
        }
        out.push(CellOutcome {
            label: format!(
                "{name} / {} / {} / expect {:?}",
                case.precondition.code(),
                case.state.code(),
                case.expected
            ),
            result,
            pending: case.pending,
        });
    }
    out
}
