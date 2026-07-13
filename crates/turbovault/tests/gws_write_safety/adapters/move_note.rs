//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection). Two precondition axes.
//!
//! **SPEC-FIRST** (design doc §10 commit 1): drives the *aspirational*
//! `GitFileTools::move_note(from, to, from_precondition, to_precondition)` — which
//! **does not exist yet**. Today's `move_file` takes a source blob hash and
//! HARDCODES `expect_absent(to)`; there is no way to pass a *destination*
//! precondition (design doc §2 defect #2). Does not compile until it lands.
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
/// desired outcome. Spec-first — every cell is active (asserts the target).
#[derive(Clone, Copy)]
struct Cell {
    precond: P,
    state: S,
    expected: O,
}

impl Cell {
    const fn new(precond: P, state: S, expected: O) -> Self {
        Self {
            precond,
            state,
            expected,
        }
    }
}

// ── SOURCE sweep — destination held absent (`to = ExpectAbsent`) ─────────────
// Same shape as delete/edit's in-place rows: the source must exist and match.
const SRC_CASES: &[Cell] = &[
    // ExpectExists (in-place default, dirty-gated)
    Cell::new(P::Exists, S::Absent, O::NoFile),
    Cell::new(P::Exists, S::CleanCommitted, O::Ok),
    Cell::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::Untracked, O::ConcurrencyError),
    // ExpectBlob(HEAD) — defined iff committed
    Cell::new(P::Head, S::CleanCommitted, O::Ok),
    Cell::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) — defined iff staged
    Cell::new(P::Index, S::CommittedStaged, O::Ok),
    Cell::new(P::Index, S::NewStaged, O::Ok),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) — SKIP where == HEAD/INDEX
    Cell::new(P::Workdir, S::CommittedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::IntentToAdd, O::Ok),
    Cell::new(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::Untracked, O::Ok),
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
const DEST_CASES: &[Cell] = &[
    // Blind → overwrite the destination unconditionally
    Cell::new(P::Blind, S::Absent, O::Ok),
    Cell::new(P::Blind, S::CleanCommitted, O::Ok),
    Cell::new(P::Blind, S::CommittedStaged, O::Ok),
    Cell::new(P::Blind, S::CommittedUnstaged, O::Ok),
    Cell::new(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Cell::new(P::Blind, S::NewStaged, O::Ok),
    Cell::new(P::Blind, S::IntentToAdd, O::Ok),
    Cell::new(P::Blind, S::NewStagedUnstaged, O::Ok),
    Cell::new(P::Blind, S::Untracked, O::Ok),
    // ExpectAbsent (clobber protection) → OK on absent dest, else refuse
    Cell::new(P::Absent, S::Absent, O::Ok),
    Cell::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Absent, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::Untracked, O::ConcurrencyError),
    // ExpectBlob(HEAD) on the dest — defined iff dest committed
    Cell::new(P::Head, S::CleanCommitted, O::Ok),
    Cell::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) on the dest — defined iff dest staged
    Cell::new(P::Index, S::CommittedStaged, O::Ok),
    Cell::new(P::Index, S::NewStaged, O::Ok),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) on the dest — SKIP where == HEAD/INDEX
    Cell::new(P::Workdir, S::CommittedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::IntentToAdd, O::Ok),
    Cell::new(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Cell::new(P::Workdir, S::Untracked, O::Ok),
    // ExpectBlob(WRONG) on the dest → refuse everywhere (incl. absent)
    Cell::new(P::Wrong, S::Absent, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];

pub fn trials() -> Vec<Trial> {
    let mut out = Vec::with_capacity(SRC_CASES.len() + DEST_CASES.len());
    for &c in SRC_CASES {
        let name = format!(
            "move_note::src::{}::{}::{:?}",
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, None, move || run_src(c)));
    }
    for &c in DEST_CASES {
        let name = format!(
            "move_note::dest::{}::{}::{:?}",
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, None, move || run_dest(c)));
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
    // Aspirational tool-layer method (does not exist yet — spec-first).
    let res = world.tools.move_note(SRC, DEST, from_pc, to_pc).await;
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
