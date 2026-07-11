//! Report aggregation (design doc §7).
//!
//! The runner is now just this: adapters (single-path or bespoke) hand back a
//! `Vec<CellOutcome>`; [`report`] tallies active pass / pending / fail, surfaces
//! every failing cell with full identity (the debuggability option (b) wanted,
//! without per-cell test fns), loudly flags any `pending` cell that has started
//! passing (un-pend it), and fails only on an ACTIVE-cell mismatch.

use super::adapter::CellOutcome;

pub fn report(op: &str, outcomes: Vec<CellOutcome>) {
    let mut active_pass = 0usize;
    let mut pending_fail = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut now_passing: Vec<String> = Vec::new();

    for c in &outcomes {
        match (&c.result, c.pending) {
            (Ok(()), None) => active_pass += 1,
            (Ok(()), Some(reason)) => now_passing.push(format!(
                "{} [pending {reason}] — now PASSES, un-pend it",
                c.label
            )),
            (Err(msg), None) => failures.push(format!("{}: {msg}", c.label)),
            (Err(_), Some(_)) => pending_fail += 1,
        }
    }

    eprintln!(
        "[gws] {op}: {active_pass} active pass, {pending_fail} pending, {} active FAIL",
        failures.len()
    );
    if !now_passing.is_empty() {
        eprintln!(
            "[gws] {op}: {} pending cell(s) now pass:\n  {}",
            now_passing.len(),
            now_passing.join("\n  ")
        );
    }
    assert!(
        failures.is_empty(),
        "[gws] {op}: {} active cell(s) failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
