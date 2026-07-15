//! The backend axis (design doc §9): set up a vault of a given write backend
//! (hermetically) and normalize whatever its layer returns into an [`Observed`].
//!
//! Only the **Git** backend is wired here (Phase 0). The **Legacy** arm lands in
//! Phase 6 (turbovault-nbl.7) as the upstream bug evidence. The state builder
//! and outcome asserter are backend-independent, so adding Legacy is additive.

use std::sync::{Arc, Once};

use tempfile::TempDir;
use turbovault_core::config::{ServerConfig, VaultConfig};
use turbovault_tools::{CommitLocks, GitFileTools};
use turbovault_vault::VaultManager;

use super::outcome::{Observed, ObservedError};

/// Make libgit2 hermetic: clear the global/system/XDG config search paths so the
/// substrate under test can't be perturbed by the developer's `~/.gitconfig`.
/// Two concrete hazards this closes: `core.autocrlf` (would rewrite line endings
/// → different blob oids → silently break the version-token contract) and
/// `core.excludesfile` (a global ignore could trip the substrate's lri gate).
/// Process-global (mirrors the substrate's own `init_libgit2_opts`); set once.
fn make_libgit2_hermetic() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for level in [
            git2::ConfigLevel::System,
            git2::ConfigLevel::Global,
            git2::ConfigLevel::XDG,
        ] {
            // SAFETY: process-global libgit2 option, set once before any repo is
            // opened on this thread; `""` clears the search path for that level.
            unsafe {
                let _ = git2::opts::set_search_path(level, "");
            }
        }
    });
}

/// A live vault under test: the temp working tree plus the write surface.
pub struct World {
    pub dir: TempDir,
    pub tools: GitFileTools,
}

impl World {
    /// A hermetic, seeded **git-backed** vault. The seed commit (from
    /// [`super::state::new_seeded_repo`]) guarantees HEAD exists even for the
    /// absent/untracked matrix states.
    pub fn git() -> World {
        make_libgit2_hermetic();
        let dir = super::state::new_seeded_repo();
        let path = dir.path().to_path_buf();
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("t", &path).build().unwrap());
        let manager = Arc::new(VaultManager::new(cfg).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let tools = GitFileTools::new(manager, path, locks);
        World { dir, tools }
    }

    /// Working-tree content of `rel` (`None` == absent).
    pub fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(rel)).ok()
    }
}

/// Normalize a substrate write `Result` (plus the post-op working-tree content)
/// into a layer-agnostic [`Observed`].
pub fn observe(result: Result<(), turbovault_core::Error>, after: Option<String>) -> Observed {
    match result {
        Ok(()) => Observed::ok(after),
        Err(e) => Observed::failed(classify(&e), after),
    }
}

fn classify(e: &turbovault_core::Error) -> ObservedError {
    use turbovault_core::Error;
    match e {
        Error::ConcurrencyError { .. } => ObservedError::Concurrency,
        Error::FileNotFound { .. } => ObservedError::NotFound,
        Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ObservedError::NotFound,
        _ => ObservedError::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::outcome::Outcome;
    use crate::harness::precondition::{Precondition, PreconditionKind};
    use crate::harness::state::{GitState, build_state};

    /// The P0 acceptance scaffold: prove the primitives compose end-to-end —
    /// `build_state` → hermetic `World` → real substrate write → `observe` →
    /// `Outcome::assert`. Uses the CURRENT write API (a blind overwrite); the
    /// P1 adapters drive the aspirational precondition surface.
    #[tokio::test]
    async fn primitives_compose_end_to_end() {
        let world = World::git();
        build_state(world.dir.path(), "note.md", GitState::CleanCommitted);

        let before = world.read("note.md");
        let res = world
            .tools
            .write_file(
                "note.md",
                "NEW",
                turbovault_tools::WriteMode::Overwrite,
                turbovault_core::Precondition::Blind,
                None,
            )
            .await;
        let after = world.read("note.md");

        let observed = observe(res, after);
        Outcome::Ok.assert(&observed, before.as_deref());
        assert_eq!(
            observed.after_content.as_deref(),
            Some("NEW"),
            "the write must land in the working tree"
        );
    }

    /// The precondition resolver reads real tokens off a built state: a WORKDIR
    /// precondition against a dirty file resolves to that file's on-disk blob.
    #[test]
    fn workdir_precondition_resolves_off_a_built_state() {
        let world = World::git();
        let oids = build_state(world.dir.path(), "d.md", GitState::CommittedUnstaged);
        match PreconditionKind::Workdir.resolve(&oids) {
            Some(Precondition::ExpectBlob(oid)) => {
                assert_eq!(
                    Some(&oid),
                    oids.workdir.as_ref(),
                    "resolves to the WORKDIR token"
                );
            }
            other => panic!(
                "expected ExpectBlob, got {other:?} ({})",
                PreconditionKind::Workdir.code()
            ),
        }
    }
}
