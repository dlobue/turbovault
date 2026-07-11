//! Per-operation adapters + the shared matrix-cell / trial machinery
//! (design doc §7).
//!
//! Each mutating op is a module here. Single-target ops share [`SinglePathOp`] +
//! [`single_path_trials`]; dual-path (`move_note`) and multi-op (`batch_execute`)
//! adapters build their own trials with [`cell_trial`].
//!
//! Each matrix cell is a `libtest-mimic` trial (turbovault-nbl.2) → its own named
//! `cargo test` entry. `pending` cells become **ignored** trials (the burndown):
//! they still run under `--ignored`, and a pass there means un-pend.
//!
//! This machinery lives with the adapters (not under `harness`) so the
//! default-harness self-test target — which pulls in `harness` — doesn't see it
//! as unused.

pub mod batch_execute;
pub mod create_from_template;
pub mod delete_note;
pub mod edit_note;
pub mod manage_tags;
pub mod move_note;
pub mod update_frontmatter;
pub mod write_note;

use libtest_mimic::{Failed, Trial};

use crate::harness::backend::World;
use crate::harness::outcome::{Observed, Outcome};
use crate::harness::precondition::{Precondition, PreconditionKind};
use crate::harness::state::{GitState, build_state};

/// The relative path every single-path op targets.
pub const REL: &str = "note.md";

/// One single-path matrix cell: a precondition selector × a working-tree state →
/// the desired outcome. `pending` marks a cell whose desired behavior isn't
/// implemented yet.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub precondition: PreconditionKind,
    pub state: GitState,
    pub expected: Outcome,
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

/// A single-target operation: it targets exactly one path, so the harness can
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
    /// Default: a successful op is enough.
    fn ok_effect(&self, _observed: &Observed) -> Result<(), String> {
        Ok(())
    }
}

/// Build a trial that runs one async cell on its own current-thread runtime.
/// `pending.is_some()` → the trial is marked `ignored` (the burndown).
pub fn cell_trial<F, Fut>(name: String, pending: Option<&'static str>, run: F) -> Trial
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut trial = Trial::test(name, move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run()).map_err(Failed::from)
    });
    if pending.is_some() {
        trial = trial.with_ignored_flag(true);
    }
    trial
}

/// One named trial per case of a single-path op. Trial name is
/// `<op>::<PRECONDITION>::<state-code>::<expected>` — e.g.
/// `write_note::WORKDIR::etc-u::Ok`.
pub fn single_path_trials<Op>(op: Op) -> Vec<Trial>
where
    Op: SinglePathOp + Copy + Send + 'static,
{
    op.cases()
        .iter()
        .map(|&case| {
            let name = format!(
                "{}::{}::{}::{:?}",
                op.name(),
                case.precondition.code(),
                case.state.code(),
                case.expected
            );
            cell_trial(name, case.pending, move || run_single_cell(op, case))
        })
        .collect()
}

async fn run_single_cell<Op: SinglePathOp>(op: Op, case: Case) -> Result<(), String> {
    let world = World::git();
    let oids = build_state(world.dir.path(), REL, case.state);
    let Some(pc) = case.precondition.resolve(&oids) else {
        return Err(format!(
            "unexpected N/A: {} token undefined in state {} — remove this cell",
            case.precondition.code(),
            case.state.code()
        ));
    };
    let before = world.read(REL);
    let observed = op.invoke(&world, REL, pc).await;
    case.expected.check(&observed, before.as_deref())?;
    if case.expected == Outcome::Ok {
        op.ok_effect(&observed)?;
    }
    Ok(())
}
