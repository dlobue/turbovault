//! The outcome axis + assertion (design doc §6).
//!
//! A backend/adapter reduces whatever its layer returns (a `Result`, a
//! `CallToolResult`, an `EditResult`) into a layer-agnostic [`Observed`]; the
//! [`Outcome`] then asserts against it. `DIRTY_ERR` and `CAS_FAIL` are the same
//! [`Outcome::ConcurrencyError`] — one unified "changed underneath us" error.

/// The desired outcome of a matrix cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Write/edit succeeds and materializes to the working tree.
    Ok,
    /// Refused with **no disk change** — the unified concurrency error the
    /// matrix labels `DIRTY_ERR` *and* `CAS_FAIL`.
    ConcurrencyError,
    /// In-place op on an absent path: `FileNotFound`, nothing created.
    NoFile,
}

/// How an operation failed, classified across layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedError {
    /// The unified "changed underneath us" refusal (precondition / dirty tree).
    Concurrency,
    /// Target does not exist.
    NotFound,
    /// Anything else (a bug from the harness's point of view — should not occur
    /// in a passing cell).
    Other,
}

/// A backend-normalized observation of what an operation did.
#[derive(Clone, Debug)]
pub struct Observed {
    /// The op reported success.
    pub succeeded: bool,
    /// Classified error kind when it failed.
    pub error: Option<ObservedError>,
    /// Working-tree content of the target *after* the op (`None` == absent).
    pub after_content: Option<String>,
}

impl Observed {
    pub fn ok(after_content: Option<String>) -> Self {
        Observed {
            succeeded: true,
            error: None,
            after_content,
        }
    }

    pub fn failed(error: ObservedError, after_content: Option<String>) -> Self {
        Observed {
            succeeded: false,
            error: Some(error),
            after_content,
        }
    }
}

impl Outcome {
    /// Assert `observed` matches this expected outcome. `before` is the target's
    /// content immediately before the op — a refusal must leave it byte-for-byte
    /// intact (the no-clobber invariant these tests exist to protect).
    ///
    /// The specific *effect* of an `Ok` (what bytes/deletion resulted) is the
    /// adapter's assertion; here `Ok` checks only that the op succeeded, so this
    /// stays operation-agnostic.
    pub fn assert(self, observed: &Observed, before: Option<&str>) {
        match self {
            Outcome::Ok => assert!(
                observed.succeeded,
                "expected OK, got failure {:?}",
                observed.error
            ),
            Outcome::ConcurrencyError => {
                assert!(
                    !observed.succeeded,
                    "expected ConcurrencyError, but the op SUCCEEDED (a clobber/defect)"
                );
                assert_eq!(
                    observed.error,
                    Some(ObservedError::Concurrency),
                    "expected a concurrency refusal"
                );
                assert_eq!(
                    observed.after_content.as_deref(),
                    before,
                    "ConcurrencyError must leave the working tree unchanged (no clobber)"
                );
            }
            Outcome::NoFile => {
                assert!(!observed.succeeded, "expected NoFile, but the op SUCCEEDED");
                assert_eq!(
                    observed.error,
                    Some(ObservedError::NotFound),
                    "expected a not-found refusal"
                );
                assert_eq!(
                    observed.after_content.as_deref(),
                    before,
                    "NoFile must not create the target"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_requires_success() {
        Outcome::Ok.assert(&Observed::ok(Some("new".into())), Some("old"));
    }

    #[test]
    #[should_panic(expected = "clobber/defect")]
    fn concurrency_error_rejects_a_silent_success() {
        // A "success" where the matrix demanded a refusal is exactly the clobber
        // defect — the assertion must catch it.
        Outcome::ConcurrencyError.assert(&Observed::ok(Some("clobbered".into())), Some("dirty"));
    }

    #[test]
    #[should_panic(expected = "unchanged")]
    fn concurrency_error_requires_no_disk_change() {
        // Correct error kind, but the content changed => still a defect.
        Outcome::ConcurrencyError.assert(
            &Observed::failed(ObservedError::Concurrency, Some("changed".into())),
            Some("dirty"),
        );
    }

    #[test]
    fn concurrency_error_accepts_refusal_with_content_intact() {
        Outcome::ConcurrencyError.assert(
            &Observed::failed(ObservedError::Concurrency, Some("dirty".into())),
            Some("dirty"),
        );
    }

    #[test]
    fn nofile_requires_notfound_and_nothing_created() {
        Outcome::NoFile.assert(&Observed::failed(ObservedError::NotFound, None), None);
    }
}
