//! Ref compare-and-swap + optimistic rebuild-on-conflict (GWS.3).
//!
//! `cas_ref` is the serialization primitive: it advances a branch ref from an
//! expected old value to a new commit **under git's ref lock**, mirroring
//! `git update-ref <new> <old>`. It is atomic and **cross-process** (the lock is
//! a lockfile in `.git`), which is the property in-process mutexes cannot give.
//!
//! `commit_with_retry` is the optimistic loop: build a commit on the current
//! tip, CAS the ref; if a concurrent writer advanced it first, re-read the tip,
//! rebuild on the new tip, and retry. The caller's builder re-runs its per-file
//! preconditions (GWS.4) on each rebuild, so a conflicting change to one of the
//! changeset's own paths surfaces as an abort rather than a silent overwrite.
//!
//! Ported to gix (GX.5): **not** via `gix::Repository::edit_reference`.
//! gix-ref 0.65's transaction reads the ref's current value BEFORE acquiring
//! its `.lock` file (`file/transaction/prepare.rs::lock_ref_and_apply_change`
//! reads `existing_ref` first, then calls `obtain_lock()`), and compares
//! against that pre-lock snapshot once the lock is held — a real TOCTOU:
//! two racing writers can both pass the compare and both write, exactly the
//! lost-update this primitive exists to prevent. (Confirmed by tracing the
//! gix-ref source and by direct multi-threaded reproduction against
//! `edit_reference`.) So `cas_ref` instead does its own lock-then-read-then-
//! compare-then-write using `gix::lock::File` directly, at the identical
//! `.lock` path gix-ref's own transaction would use — the SAME primitive
//! gix-ref builds on, just correctly ordered. Any other writer of this ref
//! (another `VaultRepo`, gix's own `edit_reference`, a human `git commit` or
//! `git gc`) contends on that same lock file, so nobody can be mid-write
//! while we read+compare+write. `commit_with_retry_n`'s loop is unchanged.
//!
//! Bypassing `edit_reference` also means bypassing its reflog write, so
//! `cas_ref` writes `logs/<refname>` (and `logs/HEAD`, when `refname` is
//! HEAD's current symbolic target) itself, using `gix::refs::log::Line` —
//! gix-ref's own reflog-line type — for the serialization format, matching
//! what `git update-ref`/`git commit` would have written.

use crate::error::{Error, Result};
use crate::oid;
use crate::repo::VaultRepo;
use git2::Oid;
use std::io::Write as _;
use std::path::Path;
use tracing::instrument;

/// How many times `commit_with_retry` rebuilds before giving up. Contention is
/// rare; this only guards against pathological live-lock.
const DEFAULT_MAX_RETRIES: u32 = 8;

impl VaultRepo {
    /// Read `refname`'s current tip via gix, discriminating an absent ref
    /// (`Ok(None)`) from a real read/decode error (`Err`).
    ///
    /// tlx.9/hq8: `.ok()` would flatten an I/O/corruption error into `None`,
    /// which on the initial-commit path (`expected_old == None`) could be
    /// misread as "ref doesn't exist yet" and let a blind advance through.
    /// `try_find_reference` already makes this distinction itself — `Ok(None)`
    /// means genuinely absent, `Err` means a real read failure. Shared by
    /// [`Self::cas_ref`] (the read under the ref's lock — the CAS comparison
    /// itself) and [`Self::commit_with_retry_n`] (the tip fed to the builder).
    fn read_ref_tip(&self, refname: &str) -> Result<Option<Oid>> {
        Ok(self
            .gix()
            .try_find_reference(refname)
            .map_err(|e| Error::other(e.to_string()))?
            .and_then(|r| r.try_id())
            .map(|id| oid::from_gix(id.detach())))
    }

    /// Atomically advance `refname` from `expected_old` to `new`, under git's
    /// ref lock (mirrors `update-ref <new> <old>`).
    ///
    /// `expected_old == None` means the ref must **not** yet exist (the
    /// initial-commit case). On any mismatch returns a `ConcurrencyError` (via
    /// [`Error::concurrency`]) with **nothing applied** — the ref is untouched.
    ///
    /// Ported to gix (GX.5): locks first, THEN reads+compares+writes — see
    /// the module doc for why `gix::Repository::edit_reference` can't be used
    /// here (it reads before it locks). `gix::lock::File` is the exact
    /// primitive gix-ref's own transaction uses internally for this same
    /// `.lock` file; this just holds it for the whole read-compare-write
    /// instead of only the write.
    #[instrument(
        skip(self),
        fields(refname = %refname, expected = ?expected_old, new = %new),
        name = "git_cas_ref"
    )]
    pub fn cas_ref(&self, refname: &str, expected_old: Option<Oid>, new: Oid) -> Result<()> {
        // ponytail: `Fail::Immediately` — no backoff/retry on lock
        // acquisition itself. Internal callers already serialize through
        // `with_commit_lock`, so contention here means a genuinely external
        // writer is mid-update; add `AfterDurationWithBackoff` if that's
        // observed to matter in practice.
        // `commondir`, not `path`: `refs/heads/*` is shared across worktrees
        // (only HEAD and a few pseudorefs are worktree-private), and
        // fan-out (GWS.9) calls `cas_ref` on `refs/heads/main` from a linked
        // scratch worktree whose `path()` is the worktree-private admin dir
        // (`.git/worktrees/<name>`), not where that ref actually lives.
        // `commondir()` is the git-dir itself for a non-worktree repo, so
        // this is correct either way.
        let common = self.git().commondir();
        let lock_target = common.join(refname);
        let mut lock = gix::lock::File::acquire_to_update_resource(
            &lock_target,
            gix::lock::acquire::Fail::Immediately,
            Some(common.to_path_buf()),
        )
        .map_err(|e| Error::other(e.to_string()))?;

        // Fresh read UNDER the lock — this IS the CAS comparison. No other
        // writer can be mid-update here: they'd need this same lock file.
        // Same NotFound-vs-real-error discrimination as `read_ref_tip`
        // always had: a corrupt/unreadable ref surfaces as `Err`, never a
        // blind "absent" that would let a bogus initial-commit CAS through.
        let current = self.read_ref_tip(refname)?;
        if current != expected_old {
            // Dropping `lock` without committing deletes the `.lock` file;
            // the ref itself is untouched.
            return Err(Error::concurrency(format!(
                "ref CAS conflict on {refname}: expected {expected_old:?}, found {current:?}"
            )));
        }

        // Reflog before the ref content itself — mirrors real git's own
        // ref-update ordering (files-backend.c writes the reflog entry
        // before the final lockfile rename), and, on any reflog-write
        // failure, aborts here with the ref still untouched rather than
        // landing an unlogged advance.
        let line = gix::refs::log::Line {
            previous_oid: expected_old
                .map(oid::to_gix)
                .unwrap_or_else(|| gix::hash::Kind::Sha1.null()),
            new_oid: oid::to_gix(new),
            signature: self.author_signature(),
            message: "turbovault-git: cas advance".into(),
        };
        self.append_reflog(common, refname, &line)?;

        writeln!(lock, "{new}")?;
        lock.commit().map_err(|e| Error::other(e.to_string()))?;
        Ok(())
    }

    /// Append `line` to `refname`'s reflog (`logs/<refname>` under the
    /// shared `common` git-dir), creating the file and its parent directory
    /// on the ref's first advance. When this worktree's HEAD is a symbolic
    /// ref pointing at `refname` (the normal "committing on the checked-out
    /// branch" case), also appends to this worktree's own private
    /// `logs/HEAD` — exactly what `git update-ref`/`git commit` do for the
    /// checked-out branch.
    fn append_reflog(
        &self,
        common: &Path,
        refname: &str,
        line: &gix::refs::log::Line,
    ) -> Result<()> {
        Self::append_reflog_line(&common.join("logs").join(refname), line)?;

        let head_points_here = self
            .gix()
            .head_name()
            .ok()
            .flatten()
            .is_some_and(|name| name.to_string() == refname);
        if head_points_here {
            Self::append_reflog_line(&self.git().path().join("logs").join("HEAD"), line)?;
        }
        Ok(())
    }

    /// Append one reflog `line` to `log_path`, creating parent directories
    /// (e.g. `logs/refs/heads/`) on first use. Reflogs are append-only.
    fn append_reflog_line(log_path: &Path, line: &gix::refs::log::Line) -> Result<()> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        line.write_to(&mut file)?;
        Ok(())
    }

    /// Advance `refname` with optimistic retry (default retry budget).
    /// See [`Self::commit_with_retry_n`].
    pub fn commit_with_retry<F>(&self, refname: &str, build: F) -> Result<Option<Oid>>
    where
        F: FnMut(Option<Oid>) -> Result<Option<Oid>>,
    {
        self.commit_with_retry_n(refname, DEFAULT_MAX_RETRIES, build)
    }

    /// Advance `refname` with optimistic retry. `build` is called with the
    /// current tip (the parent to build on, `None` if the branch is unborn) and
    /// returns `Some(commit)` to CAS onto that tip, or `None` to signal a
    /// **no-op** — there is nothing to commit (e.g. the resulting tree is
    /// identical to the base), so the ref is left untouched and the method
    /// returns `Ok(None)`. If the CAS loses to a concurrent advance, the tip is
    /// re-read and `build` is called again on the new tip, up to `max_retries`
    /// rebuilds.
    ///
    /// The builder owns conflict policy: on a rebuild it re-validates its
    /// per-file preconditions against the new tip (GWS.4) and may itself return
    /// an error to abort (the reconsideration domino) instead of rebuilding.
    #[instrument(
        skip(self, build),
        fields(refname = %refname, max_retries),
        name = "git_commit_with_retry"
    )]
    pub fn commit_with_retry_n<F>(
        &self,
        refname: &str,
        max_retries: u32,
        mut build: F,
    ) -> Result<Option<Oid>>
    where
        F: FnMut(Option<Oid>) -> Result<Option<Oid>>,
    {
        for _ in 0..=max_retries {
            // tlx.9: same NotFound-vs-real-error discrimination as cas_ref — an
            // unborn ref is `None`, but a real read error must surface, not
            // masquerade as "branch has no commits yet".
            let tip = self.read_ref_tip(refname)?;
            // `None` from the builder = no-op (e.g. an identity tree): nothing
            // to commit, so skip the CAS and leave the ref where it is.
            let new = match build(tip)? {
                Some(oid) => oid,
                None => return Ok(None),
            };
            match self.cas_ref(refname, tip, new) {
                Ok(()) => return Ok(Some(new)),
                // Lost the race: the ref moved between our read and the lock.
                // Re-read the tip and rebuild on it.
                Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. })) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(Error::other(format!(
            "ref CAS exhausted {max_retries} retries on {refname} (excessive contention)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plumbing::TreeChange;
    use git2::Repository;
    use std::cell::Cell;
    use tempfile::TempDir;

    const MAIN: &str = "refs/heads/main";

    fn open_unborn() -> (TempDir, VaultRepo) {
        let tmp = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(tmp.path(), &opts).unwrap();
        let vr = VaultRepo::open(tmp.path()).unwrap();
        (tmp, vr)
    }

    fn upsert(path: &str, content: &str) -> TreeChange {
        TreeChange::Upsert {
            path: path.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    /// Build a commit on `parent` (or initial if None) carrying one upsert.
    fn build_on(vr: &VaultRepo, parent: Option<Oid>, path: &str, content: &str) -> Oid {
        let base = parent.map(|p| vr.git().find_commit(p).unwrap().tree_id());
        let tree = vr.build_tree(base, &[upsert(path, content)]).unwrap();
        let parents: Vec<Oid> = parent.into_iter().collect();
        vr.commit_tree(tree, &parents, "c").unwrap()
    }

    #[test]
    fn cas_ref_initial_then_advance() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0)
            .expect("initial CAS (None -> c0)");
        assert_eq!(vr.head_oid(), Some(c0));

        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        vr.cas_ref(MAIN, Some(c0), c1).expect("advance c0 -> c1");
        assert_eq!(vr.head_oid(), Some(c1));
    }

    /// turbovault-y1r.6: `cas_ref` must write a reflog entry for every
    /// advance — bypassing `gix::Repository::edit_reference` (see the
    /// module doc) means gix-ref's own reflog write is bypassed too, and
    /// this is the substrate's only recovery instrument on a git-backed
    /// vault (`git reflog`; the audit/rollback tools refuse there). Checks
    /// both `logs/refs/heads/main` (the ref's own log) and `logs/HEAD`
    /// (since HEAD points at `main` for both advances here).
    #[test]
    fn cas_ref_writes_reflog_entries() {
        let (tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();
        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        vr.cas_ref(MAIN, Some(c0), c1).unwrap();

        let branch_log =
            std::fs::read_to_string(tmp.path().join(".git/logs/refs/heads/main")).unwrap();
        let head_log = std::fs::read_to_string(tmp.path().join(".git/logs/HEAD")).unwrap();

        let zero = "0".repeat(40);
        assert!(
            branch_log.contains(&format!("{zero} {c0} ")),
            "initial CAS (null -> c0) logged: {branch_log}"
        );
        assert!(
            branch_log.contains(&format!("{c0} {c1} ")),
            "advance (c0 -> c1) logged: {branch_log}"
        );
        assert!(
            branch_log.matches("turbovault-git: cas advance").count() == 2,
            "both advances carry the CAS reflog message: {branch_log}"
        );
        assert_eq!(
            head_log, branch_log,
            "HEAD's reflog mirrors the branch it symbolically points at"
        );
    }

    /// turbovault-y1r.6 companion: a ref that is NOT HEAD's current target
    /// must get its own reflog but must NOT touch `logs/HEAD` — otherwise
    /// every fan-out / scratch-worktree ref advance would pollute the main
    /// worktree's HEAD history.
    #[test]
    fn cas_ref_on_non_head_ref_does_not_touch_head_reflog() {
        let (tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref("refs/heads/other", None, c0).unwrap();

        assert!(
            tmp.path().join(".git/logs/refs/heads/other").exists(),
            "the advanced ref's own reflog is written"
        );
        assert!(
            !tmp.path().join(".git/logs/HEAD").exists(),
            "HEAD reflog untouched: HEAD (unborn `main`) never pointed at `other`"
        );
    }

    /// hq8 (tlx.9 follow-up): real fault injection — corrupt a throwaway `.git`
    /// loose ref so `refname_to_id` fails with a NON-NotFound error, and assert
    /// `cas_ref` / `commit_with_retry` SURFACE it instead of swallowing to
    /// `None` (a blind "ref absent"). This is what `.ok()` used to do; it kills
    /// the NotFound-match-guard mutation survivors without mocking the git2 API.
    #[test]
    fn corrupt_ref_surfaces_error_instead_of_silent_absent() {
        let (tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();
        drop(vr); // release the handle before corrupting the ref on disk

        // Malformed oid in the loose ref → libgit2 parse error (not NotFound).
        std::fs::write(tmp.path().join(".git/refs/heads/main"), "not-a-valid-oid\n").unwrap();
        let vr = VaultRepo::open(tmp.path()).unwrap();

        // Precondition: the corruption really yields an `Err` (not `Ok(None)`,
        // "absent") via GIX's `try_find_reference` — the read path `cas_ref`
        // and `commit_with_retry_n` now exercise (GX.5) — else the guard would
        // legitimately map it to `None` and this proves nothing.
        assert!(
            vr.gix().try_find_reference(MAIN).is_err(),
            "corruption must produce a gix read error, not Ok(None) (absent)"
        );

        // commit_with_retry resolves the tip via the same gix read first, so a
        // non-absent error must abort, not be treated as an unborn branch.
        let res = vr.commit_with_retry_n(MAIN, 0, |_tip| Ok(None));
        assert!(
            res.is_err(),
            "commit_with_retry must surface the ref-read error, not swallow to None"
        );

        // cas_ref's read-under-lock must do the same.
        let some = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
        assert!(
            vr.cas_ref(MAIN, None, some).is_err(),
            "cas_ref must surface the ref-read error, not blind-write a 'new' ref"
        );
    }

    #[test]
    fn cas_ref_rejects_stale_and_leaves_ref() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let bogus = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        match vr.cas_ref(MAIN, Some(bogus), c1) {
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) => {
                assert!(
                    reason.contains(&c0.to_string()),
                    "reason must name the found oid: {reason}"
                );
            }
            other => panic!("expected ConcurrencyError, got {other:?}"),
        }
        assert_eq!(vr.head_oid(), Some(c0), "ref unchanged on reject");
    }

    #[test]
    fn cas_ref_initial_rejects_when_ref_exists() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();
        // expected_old = None means "must not exist", but it does now.
        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        assert!(matches!(
            vr.cas_ref(MAIN, None, c1),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
        ));
    }

    /// turbovault-a0l (PERF-1 safety guard): a REUSED `VaultRepo` handle must
    /// still observe a ref advance made by a DIFFERENT handle (another process)
    /// under `lock_ref`. If libgit2's refdb served a stale cached tip, handle A
    /// would clobber B's commit — a cross-process lost update, the exact failure
    /// the substrate exists to prevent. This is the pivotal correctness question
    /// for caching the repo handle (PERF-1): if it fails, caching is unsafe.
    #[test]
    fn reused_handle_detects_external_ref_advance_no_lost_update() {
        let (tmp, vr_a) = open_unborn();
        // A makes the initial commit (populates A's refdb with c0).
        let c0 = build_on(&vr_a, None, "a.md", "v1");
        vr_a.cas_ref(MAIN, None, c0).unwrap();
        assert_eq!(vr_a.head_oid(), Some(c0));

        // B = a SEPARATE handle (mimics another process) advances main.
        let vr_b = VaultRepo::open(tmp.path()).unwrap();
        let c1 = build_on(&vr_b, Some(c0), "b.md", "from-B");
        vr_b.cas_ref(MAIN, Some(c0), c1).unwrap();

        // A, REUSING its handle, advances main. commit_with_retry reads the tip
        // and CAS-locks; correct behavior is to see c1 and commit on top of it,
        // never clobber it from a stale c0.
        let got = vr_a
            .commit_with_retry(MAIN, |tip| Ok(Some(build_on(&vr_a, tip, "c.md", "from-A"))))
            .unwrap()
            .expect("a commit was produced");
        let parent = vr_a.git().find_commit(got).unwrap().parent_id(0).unwrap();
        assert_eq!(
            parent, c1,
            "reused handle committed atop B's external advance (saw the ref change; no lost update)"
        );
        assert!(
            vr_a.git().find_commit(c1).is_ok(),
            "B's commit is still reachable, not clobbered"
        );
    }

    #[test]
    fn commit_with_retry_no_contention() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let got = vr
            .commit_with_retry(MAIN, |tip| Ok(Some(build_on(&vr, tip, "b.md", "b"))))
            .unwrap()
            .expect("a commit was produced");
        assert_eq!(vr.head_oid(), Some(got));
    }

    #[test]
    fn commit_with_retry_rebuilds_on_conflict() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let calls = Cell::new(0u32);
        let got = vr
            .commit_with_retry(MAIN, |tip| {
                calls.set(calls.get() + 1);
                let tip = tip.unwrap();
                // On the FIRST attempt only, a concurrent writer advances the ref
                // behind our back so our CAS must lose and rebuild.
                if calls.get() == 1 {
                    let concurrent = build_on(&vr, Some(tip), "concurrent.md", "x");
                    vr.cas_ref(MAIN, Some(tip), concurrent).unwrap();
                }
                Ok(Some(build_on(&vr, Some(tip), "mine.md", "m")))
            })
            .unwrap()
            .expect("a commit was produced");

        assert_eq!(calls.get(), 2, "exactly one rebuild after the conflict");
        assert_eq!(vr.head_oid(), Some(got));
        // We rebuilt on the concurrent tip, so the final tree carries BOTH files.
        let head_tree = vr.git().find_commit(got).unwrap().tree_id();
        assert!(
            vr.blob_oid_at(head_tree, "concurrent.md")
                .unwrap()
                .is_some()
        );
        assert!(vr.blob_oid_at(head_tree, "mine.md").unwrap().is_some());
    }

    /// turbovault-uag: relentless contention exhausts the retry budget and
    /// surfaces a loud error (the live-lock guard) rather than spinning forever
    /// or silently giving up. Every attempt loses the CAS because a concurrent
    /// writer advances the ref first.
    #[test]
    fn commit_with_retry_exhausts_under_relentless_contention() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let calls = Cell::new(0u32);
        let err = vr
            .commit_with_retry_n(MAIN, 2, |tip| {
                calls.set(calls.get() + 1);
                let tip = tip.unwrap();
                // Advance the ref behind our back BEFORE our CAS, every attempt.
                let concurrent = build_on(&vr, Some(tip), &format!("c{}.md", calls.get()), "x");
                vr.cas_ref(MAIN, Some(tip), concurrent).unwrap();
                Ok(Some(build_on(&vr, Some(tip), "mine.md", "m")))
            })
            .unwrap_err();

        // max_retries=2 -> the loop runs 0..=2 = 3 attempts, all lose.
        assert_eq!(
            calls.get(),
            3,
            "builder runs max_retries+1 times then gives up"
        );
        assert!(
            err.to_string().contains("exhausted") && err.to_string().contains("contention"),
            "loud exhaustion error: {err}"
        );
    }

    /// turbovault-xw4: real-thread contention. N threads each open their OWN
    /// VaultRepo (sharing the CommitLocks registry) and commit a DISTINCT file
    /// concurrently. Every commit must land — the per-worktree commit lock +
    /// update-ref CAS serialize them with NO lost update. The whole "no lost
    /// update under contention" thesis was previously asserted only via
    /// sequential simulated races; this drives genuine threads.
    #[test]
    fn parallel_commit_changeset_lands_every_commit() {
        let (tmp, vr0) = open_unborn();
        vr0.commit_changeset(&turbovault_core::ChangePlan::new("seed").create("seed.md", "0"))
            .unwrap();
        let path = tmp.path().to_path_buf();
        let locks = vr0.commit_locks();
        drop(vr0);

        let n = 8u32;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let p = path.clone();
                let l = std::sync::Arc::clone(&locks);
                std::thread::spawn(move || {
                    let vr = crate::VaultRepo::open_with_locks(&p, l).unwrap();
                    vr.commit_changeset(
                        &turbovault_core::ChangePlan::new("c").create(format!("f{i}.md"), "x"),
                    )
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        // Every file + the seed is in HEAD's tree — nothing lost to a race.
        let vr = crate::VaultRepo::open_with_locks(&path, locks).unwrap();
        let tree = vr
            .git()
            .find_commit(vr.head_oid().unwrap())
            .unwrap()
            .tree_id();
        assert!(vr.blob_oid_at(tree, "seed.md").unwrap().is_some());
        for i in 0..n {
            assert!(
                vr.blob_oid_at(tree, &format!("f{i}.md")).unwrap().is_some(),
                "f{i}.md must have landed"
            );
        }
    }
}
