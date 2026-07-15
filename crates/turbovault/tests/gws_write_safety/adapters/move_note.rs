//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection). Two precondition axes.
//!
//! nbl.6 cutover: drives the real `GitFileTools::move_file(from, to,
//! precondition, message)`, which honors the SOURCE precondition but HARDCODES
//! `expect_absent(to)` — there is no way to pass a *destination* precondition yet
//! (design doc §2 defect #2). So the whole **dest** sweep, plus the source-side
//! dirty/precond-vs-workdir cells, are `pending` on the dual-path move burndown
//! (turbovault-9n6); the source sweep's clean cells run live.
//!
//! The matrix's two row groups are two independent 1-D sweeps:
//! - **source** sweep: vary the source state × source precondition, destination
//!   held ABSENT with `to = ExpectAbsent`. Same shape as delete/edit's in-place
//!   rows (the source is the removed target).
//! - **dest** sweep: vary the destination state × destination precondition,
//!   source held CLEAN with `from = ExpectExists`. The destination is the write
//!   target — `ExpectAbsent` on it is the clobber protection.

use libtest_mimic::Trial;

use super::cell_trial;
use crate::harness::backend::{World, observe};
use crate::harness::outcome::{ObservedError, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::{GitState as S, build_state};

const SRC: &str = "from.md";
const DEST: &str = "to.md";

/// One cell of a single move sweep: the varied path's state × precondition →
/// desired outcome. `pending` marks a cell whose desired behavior isn't wired
/// yet (the burndown) — an `ignored` trial, exactly like [`super::Case`].
#[derive(Clone, Copy)]
struct Cell {
    precond: P,
    state: S,
    expected: O,
    pending: Option<&'static str>,
}

impl Cell {
    const fn new(precond: P, state: S, expected: O) -> Self {
        Self {
            precond,
            state,
            expected,
            pending: None,
        }
    }

    const fn pending(precond: P, state: S, expected: O, reason: &'static str) -> Self {
        Self {
            precond,
            state,
            expected,
            pending: Some(reason),
        }
    }
}

// ── SOURCE sweep — destination held absent (`to = ExpectAbsent`) ─────────────
// Same shape as delete/edit's in-place rows: the source must exist and match.
const SRC_CASES: &[Cell] = &[
    // ExpectExists (in-place default, dirty-gated) — same pending set as the
    // other in-place ops (the source is the removed target).
    Cell::new(P::Exists, S::Absent, O::NoFile),
    Cell::new(P::Exists, S::CleanCommitted, O::Ok),
    Cell::pending(
        P::Exists,
        S::CommittedStaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Cell::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Cell::pending(
        P::Exists,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Cell::pending(P::Exists, S::NewStaged, O::ConcurrencyError, DIRTY_GATE),
    Cell::pending(P::Exists, S::IntentToAdd, O::ConcurrencyError, DIRTY_GATE),
    Cell::pending(
        P::Exists,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Cell::pending(P::Exists, S::Untracked, O::ConcurrencyError, DIRTY_GATE),
    // ExpectBlob(HEAD) — defined iff committed
    Cell::new(P::Head, S::CleanCommitted, O::Ok),
    Cell::pending(
        P::Head,
        S::CommittedStaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Cell::pending(
        P::Head,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Cell::pending(
        P::Head,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    // ExpectBlob(INDEX) — defined iff staged
    Cell::pending(P::Index, S::CommittedStaged, O::Ok, PRECOND_VS_HEAD),
    Cell::pending(P::Index, S::NewStaged, O::Ok, PRECOND_VS_HEAD),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) — SKIP where == HEAD/INDEX
    Cell::pending(P::Workdir, S::CommittedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Cell::pending(
        P::Workdir,
        S::CommittedStagedUnstaged,
        O::Ok,
        PRECOND_VS_HEAD,
    ),
    Cell::pending(P::Workdir, S::IntentToAdd, O::Ok, PRECOND_VS_HEAD),
    Cell::pending(P::Workdir, S::NewStagedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Cell::pending(P::Workdir, S::Untracked, O::Ok, PRECOND_VS_HEAD),
    // ExpectBlob(WRONG) → refuse everywhere; NoFile on absent
    Cell::new(P::Wrong, S::Absent, O::NoFile),
    Cell::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];

// ── DEST sweep — source held clean committed (`from = ExpectExists`) ─────────
// The destination is the write target; `ExpectAbsent` on it is the clobber guard.
// The destination precondition is not expressible against today's `move_file`
// (it hardcodes `expect_absent(to)`), so every cell whose desired outcome
// diverges from that hardcoded behavior is `pending` on the dual-path move
// burndown (turbovault-9n6). Cells that happen to coincide with `expect_absent`
// stay active.
const DEST_CASES: &[Cell] = &[
    // Blind → overwrite the destination unconditionally. Coincides with
    // expect_absent only where the dest is absent-in-HEAD (absent / uncommitted).
    Cell::new(P::Blind, S::Absent, O::Ok),
    Cell::pending(P::Blind, S::CleanCommitted, O::Ok, DEST_PRECOND),
    Cell::pending(P::Blind, S::CommittedStaged, O::Ok, DEST_PRECOND),
    Cell::pending(P::Blind, S::CommittedUnstaged, O::Ok, DEST_PRECOND),
    Cell::pending(P::Blind, S::CommittedStagedUnstaged, O::Ok, DEST_PRECOND),
    Cell::new(P::Blind, S::NewStaged, O::Ok),
    Cell::new(P::Blind, S::IntentToAdd, O::Ok),
    Cell::new(P::Blind, S::NewStagedUnstaged, O::Ok),
    Cell::new(P::Blind, S::Untracked, O::Ok),
    // ExpectAbsent (clobber protection) → OK on absent dest, else refuse. The
    // uncommitted-but-present dest wrongly passes expect_absent-vs-HEAD today.
    Cell::new(P::Absent, S::Absent, O::Ok),
    Cell::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::pending(P::Absent, S::NewStaged, O::ConcurrencyError, DEST_PRECOND),
    Cell::pending(P::Absent, S::IntentToAdd, O::ConcurrencyError, DEST_PRECOND),
    Cell::pending(
        P::Absent,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DEST_PRECOND,
    ),
    Cell::pending(P::Absent, S::Untracked, O::ConcurrencyError, DEST_PRECOND),
    // ExpectBlob(HEAD) on the dest — defined iff dest committed
    Cell::pending(P::Head, S::CleanCommitted, O::Ok, DEST_PRECOND),
    Cell::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) on the dest — defined iff dest staged
    Cell::pending(P::Index, S::CommittedStaged, O::Ok, DEST_PRECOND),
    Cell::new(P::Index, S::NewStaged, O::Ok),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::pending(
        P::Index,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DEST_PRECOND,
    ),
    // ExpectBlob(WORKDIR) on the dest — SKIP where == HEAD/INDEX
    Cell::pending(P::Workdir, S::CommittedUnstaged, O::Ok, DEST_PRECOND),
    Cell::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok, DEST_PRECOND),
    Cell::new(P::Workdir, S::IntentToAdd, O::Ok),
    Cell::new(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::Untracked, O::Ok),
    // ExpectBlob(WRONG) on the dest → refuse everywhere (incl. absent). Where
    // the dest is absent-in-HEAD, expect_absent passes today → wrongly Ok.
    Cell::pending(P::Wrong, S::Absent, O::ConcurrencyError, DEST_PRECOND),
    Cell::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::pending(P::Wrong, S::NewStaged, O::ConcurrencyError, DEST_PRECOND),
    Cell::pending(P::Wrong, S::IntentToAdd, O::ConcurrencyError, DEST_PRECOND),
    Cell::pending(
        P::Wrong,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DEST_PRECOND,
    ),
    Cell::pending(P::Wrong, S::Untracked, O::ConcurrencyError, DEST_PRECOND),
];

// Burndown reasons (nbl.8 / 9n6) — the aspirational behavior the cutover defers.
const DIRTY_GATE: &str = "GWS: no dirty gate for move source (discards/uses uncommitted content)";
const HEAD_CLOBBER: &str =
    "GWS: dirty-tree clobber — HEAD token passes vs HEAD, move uses dirty source bytes";
const PRECOND_VS_HEAD: &str = "GWS: precondition checked vs HEAD, not the working tree";
const DEST_PRECOND: &str = "GWS: destination precondition not wired — move_file hardcodes expect_absent(to) (dual-path move is turbovault-9n6)";

pub fn trials() -> Vec<Trial> {
    let mut out = Vec::with_capacity(SRC_CASES.len() + DEST_CASES.len());
    for &c in SRC_CASES {
        let name = format!(
            "move_note::src::{}::{}::{:?}",
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, c.pending, move || run_src(c)));
    }
    for &c in DEST_CASES {
        let name = format!(
            "move_note::dest::{}::{}::{:?}",
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, c.pending, move || run_dest(c)));
    }
    out
}

/// Source sweep: vary the source, hold the destination absent.
async fn run_src(c: Cell) -> Result<(), String> {
    let world = World::git();
    let src_oids = build_state(world.dir.path(), SRC, c.state);
    build_state(world.dir.path(), DEST, S::Absent);
    let Some(from_pc) = c.precond.resolve(&src_oids) else {
        return Err(format!(
            "unexpected N/A: source {} token undefined in state {}",
            c.precond.code(),
            c.state.code()
        ));
    };
    run_move(c.expected, &world, from_pc, Precondition::ExpectAbsent).await
}

/// Dest sweep: vary the destination, hold the source clean+committed.
async fn run_dest(c: Cell) -> Result<(), String> {
    let world = World::git();
    build_state(world.dir.path(), SRC, S::CleanCommitted);
    let dest_oids = build_state(world.dir.path(), DEST, c.state);
    let Some(to_pc) = c.precond.resolve(&dest_oids) else {
        return Err(format!(
            "unexpected N/A: dest {} token undefined in state {}",
            c.precond.code(),
            c.state.code()
        ));
    };
    run_move(c.expected, &world, Precondition::ExpectExists, to_pc).await
}

/// Invoke the aspirational move and assert the dual-path outcome: an OK leaves
/// the source gone and the destination present; a refusal leaves BOTH paths
/// byte-for-byte intact (no partial move, no dest clobber).
async fn run_move(
    expected: O,
    world: &World,
    from_pc: Precondition,
    to_pc: Precondition,
) -> Result<(), String> {
    let before_src = world.read(SRC);
    let before_dest = world.read(DEST);
    // nbl.6 cutover: today's `move_file` honors only the SOURCE precondition and
    // HARDCODES `expect_absent(to)`. A caller-controllable DESTINATION
    // precondition is the deferred dual-path burndown (turbovault-9n6), so the
    // dest sweep's `to_pc` cannot be expressed yet — those cells are `pending`.
    let _ = to_pc;
    let res = world.tools.move_file(SRC, DEST, from_pc, None).await;
    let after_src = world.read(SRC);
    let after_dest = world.read(DEST);
    let obs = observe(res, after_src.clone());

    match expected {
        O::Ok => {
            if !obs.succeeded {
                return Err(format!("expected Ok move, got failure {:?}", obs.error));
            }
            if after_src.is_some() {
                return Err(format!("OK move: source still present: {after_src:?}"));
            }
            if after_dest.is_none() {
                return Err("OK move: destination missing after move".into());
            }
        }
        O::ConcurrencyError => {
            if obs.succeeded {
                return Err("expected ConcurrencyError, move SUCCEEDED (a clobber/defect)".into());
            }
            if obs.error != Some(ObservedError::Concurrency) {
                return Err(format!(
                    "expected a concurrency refusal, got {:?}",
                    obs.error
                ));
            }
            if after_src != before_src {
                return Err("ConcurrencyError move: source changed (must be intact)".into());
            }
            if after_dest != before_dest {
                return Err("ConcurrencyError move: destination changed (a clobber)".into());
            }
        }
        O::NoFile => {
            if obs.succeeded {
                return Err("expected NoFile, move SUCCEEDED".into());
            }
            if obs.error != Some(ObservedError::NotFound) {
                return Err(format!("expected a not-found refusal, got {:?}", obs.error));
            }
            if after_src != before_src || after_dest != before_dest {
                return Err("NoFile move: a path changed".into());
            }
        }
        O::OpError => return Err("move_note has no OpError cells".into()),
    }
    Ok(())
}
