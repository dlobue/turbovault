//! Fan-out worktree mode (GWS.9).
//!
//! `begin_fanout` opens a **scratch git worktree** on a `wip/<id>` branch
//! forked from main's current tip. All changesets applied through that
//! worktree's [`VaultRepo`] commit to the wip branch — they share the parent's
//! object DB but use a separate working tree + index. Obsidian, pointed at
//! main's working tree, stays stable for the whole fan-out.
//!
//! When the fan-out is done, `commit_fanout` merges the wip branch back into
//! main (configurable strategy) and cleans up the scratch worktree + branch.
//! `abandon_fanout` discards the fan-out (no commits land on main).
//!
//! The fan-out's worktree gets a separate per-worktree commit mutex (it's a
//! different worktree key), so changesets inside the fan-out never contend
//! with main's commit mutex.

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::Oid;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tracing::instrument;

// -------- GX.10: `git worktree` CLI subprocess helpers --------
//
// Neither git2 nor gix expose a worktree-admin *mutation* API (create/
// remove) — gix is read-only here, and git2's is being retired crate-wide.
// So worktree create/remove shell out to the `git worktree` CLI below.
// Reads (the orphan scan, `list_worktrees`) also go through the CLI rather
// than gix's native `Repository::worktrees()` proxy: gix's proxy exposes the
// worktree's *admin directory name* (`.git/worktrees/<name>`), but plain
// `git worktree add <path> <branch>` has no flag to control that name — git
// derives it from the path's basename, which callers (the MCP layer, tests)
// choose independently of our `wip-<id>` convention. The `branch` field in
// `git worktree list --porcelain` output is the one piece of worktree
// metadata that's still under our control (we name the branch `wip/<id>`
// ourselves), so it's the only reliable signal for "is this one of ours" —
// hence the same CLI/porcelain path is used for every worktree-admin read
// and write in this module, rather than splitting reads onto gix and
// mutations onto the CLI.

/// Run `git -C <repo_root> <args>`, capturing output. Hermetic in three ways:
/// (1) neutralizes host git config (`GIT_CONFIG_*`) so behavior doesn't depend
/// on `~/.gitconfig`; (2) clears the ambient `GIT_DIR`/`GIT_WORK_TREE`/etc. env
/// vars, which git's repo discovery consults BEFORE honoring `-C` — an
/// inherited one (from a wrapper, hook, or dotfiles-bare-repo alias) would
/// silently retarget every worktree op at the wrong repository, a hazard the
/// old explicit-path git2 API never had; (3) pins the C locale so git's
/// `fatal()` wording stays English for `is_unknown_worktree`'s match. Same env
/// discipline as the WSS harness's `git()` helper (state.rs).
fn run_git(repo_root: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("LANGUAGE", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .output()?)
}

/// A non-zero exit from a `git worktree` subprocess, mapped to a substrate
/// error — never silently ignored. Includes the command + stderr so a failed
/// create/remove surfaces loudly.
fn git_err(args: &[&str], out: &Output) -> Error {
    Error::other(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The working-tree root to run `git -C` against. Mirrors `VaultRepo::
/// worktree_key`'s bare-repo fallback (private to repo.rs — recomputed here
/// rather than exposing it, since worktree.rs is the only other consumer).
fn repo_root(main: &VaultRepo) -> PathBuf {
    main.git()
        .workdir()
        .unwrap_or_else(|| main.git().path())
        .to_path_buf()
}

/// Whether a failed `git worktree remove` means "already gone" — the CLI
/// equivalent of git2's `ErrorCode::NotFound` on `find_worktree`. Matches
/// git's stable fatal() wording for a path it doesn't recognize as a
/// worktree; every other failure (locked, dirty, main worktree) has
/// different wording and surfaces loudly instead.
fn is_unknown_worktree(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("is not a working tree")
}

/// One entry from `git worktree list --porcelain`.
struct WtEntry {
    path: PathBuf,
    /// `Some("refs/heads/<name>")`, or `None` for a detached checkout.
    branch: Option<String>,
}

/// Enumerate every worktree (main + linked) registered against `repo_root`,
/// via `git worktree list --porcelain -z` (`-z` NUL-terminates fields so a
/// path containing a newline can't be misparsed as a record boundary).
/// Pure read.
fn list_worktrees(repo_root: &Path) -> Result<Vec<WtEntry>> {
    let args = ["worktree", "list", "--porcelain", "-z"];
    let out = run_git(repo_root, &args)?;
    if !out.status.success() {
        return Err(git_err(&args, &out));
    }
    // Records are separated by a blank porcelain line, which under `-z`
    // shows up as a doubled NUL (each field, including the blank one, is
    // itself NUL-terminated).
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    for record in text.split("\0\0") {
        if record.is_empty() {
            continue;
        }
        let mut path = None;
        let mut branch = None;
        for line in record.split('\0') {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.to_string());
            }
        }
        if let Some(path) = path {
            entries.push(WtEntry { path, branch });
        }
    }
    Ok(entries)
}

/// How a fan-out merges back into main.
#[derive(Debug, Clone, Copy)]
pub enum MergeStrategy {
    /// `git merge --no-ff` — a merge commit on main with main's tip and the
    /// wip tip as parents. Preserves the wip branch's per-changeset commits
    /// (no squash). The default.
    MergeCommit,
    /// Advance main's ref directly to the wip tip — fails if main has moved
    /// since the fan-out began (would create a fork; use `MergeCommit` then).
    FastForward,
}

/// Result of a successful merge-back.
#[derive(Debug, Clone)]
pub struct MergeBackResult {
    /// Main's tip after the merge-back.
    pub tip_after: Oid,
    /// Main's tip before the merge-back (the CAS expected-old).
    pub tip_before: Oid,
    /// `Some(oid)` of the merge commit (`MergeCommit` strategy); `None` for a
    /// pure fast-forward (no new commit object, just a ref advance).
    pub merge_commit: Option<Oid>,
}

/// Stateless handle to an open fan-out scratch worktree — everything needed
/// to merge OR abandon the fan-out later, without holding a borrowed
/// [`FanoutWorktree`] across the wait (e.g. between MCP tool calls).
///
/// Returned by [`VaultRepo::open_fanout_worktree`]; consumed by
/// [`VaultRepo::merge_fanout_back`] and [`VaultRepo::abandon_fanout_by_info`].
/// The MCP layer uses this triple; the in-process programmatic API
/// ([`FanoutWorktree`]) wraps the same info for ergonomic borrowing.
#[derive(Debug, Clone)]
pub struct FanoutInfo {
    pub wip_branch: String,
    pub worktree_name: String,
    pub worktree_path: PathBuf,
    pub parent_tip: Oid,
    pub main_branch: String,
}

/// An open fan-out scratch worktree. Hold txns through `worktree_repo()`;
/// finalize via `commit_fanout` (merge back) or `abandon_fanout` (discard).
pub struct FanoutWorktree<'a> {
    main: &'a VaultRepo,
    worktree_repo: VaultRepo,
    info: FanoutInfo,
}

impl<'a> FanoutWorktree<'a> {
    /// The substrate handle for changesets inside the fan-out.
    pub fn worktree_repo(&self) -> &VaultRepo {
        &self.worktree_repo
    }

    /// The wip branch the fan-out commits to (`wip/<id>`).
    pub fn wip_branch(&self) -> &str {
        &self.info.wip_branch
    }

    /// Main's tip when the fan-out began.
    pub fn parent_tip(&self) -> Oid {
        self.info.parent_tip
    }

    /// The full info handle (stateless; usable by `merge_fanout_back` /
    /// `abandon_fanout_by_info` when this `FanoutWorktree`'s borrow ends).
    pub fn info(&self) -> &FanoutInfo {
        &self.info
    }

    /// Merge the fan-out back into main and clean up the scratch worktree.
    /// On any error the fan-out artifacts may be left behind; call
    /// `abandon_fanout` to clean up explicitly.
    #[instrument(
        skip(self),
        fields(
            wip_branch = %self.info.wip_branch,
            main_branch = %self.info.main_branch,
            strategy = ?strategy,
        ),
        name = "git_commit_fanout"
    )]
    pub fn commit_fanout(self, strategy: MergeStrategy) -> Result<MergeBackResult> {
        self.commit_fanout_with_message(strategy, None)
    }

    /// turbovault-b1q: like [`Self::commit_fanout`] but with a caller-supplied
    /// merge-commit subject (used by the MCP `commit_transaction` tool's
    /// `commit_message`). `None` falls back to the auto-derived message.
    pub fn commit_fanout_with_message(
        self,
        strategy: MergeStrategy,
        message: Option<&str>,
    ) -> Result<MergeBackResult> {
        // Delegate to the stateless API so behavior is identical to the
        // MCP path. `main.merge_fanout_back` does the lock + merge + cleanup.
        self.main.merge_fanout_back(&self.info, strategy, message)
    }

    /// Discard the fan-out: nothing lands on main; scratch worktree + wip
    /// branch removed.
    pub fn abandon_fanout(self) -> Result<()> {
        self.main.abandon_fanout_by_info(&self.info)
    }
}

/// Remove the scratch worktree + delete the wip branch. Tolerant: try every
/// step; if one fails we still attempt the rest, then return the first error.
fn cleanup_inner(main: &VaultRepo, wip_branch: &str, worktree_path: &Path) -> Result<()> {
    let repo = main.git();
    let repo_root = repo_root(main);
    let mut first_err: Option<Error> = None;

    // (a) Remove the scratch worktree: `git worktree remove` unregisters the
    // `.git/worktrees/<name>/` metadata AND deletes the working-tree
    // directory in one call. `--force --force` matches the old git2 prune
    // options (`valid(true).working_tree(true).locked(true)`): remove even
    // if the worktree still looks checked-out-clean or locked (our fanouts
    // are never locked, but stay tolerant the same way the old code was).
    // Tolerate "already gone" (git doesn't recognize the path as a
    // worktree) — the CLI equivalent of the old code's `ErrorCode::NotFound`
    // tolerance on `find_worktree`.
    let path_str = worktree_path.to_string_lossy();
    let rm_args = [
        "worktree",
        "remove",
        "--force",
        "--force",
        // `--` end-of-options: a scratch path that begins with `-` must not be
        // misparsed as a flag (defensive — current callers build absolute
        // temp-dir paths, but this is `pub` API with no path validation).
        "--",
        path_str.as_ref(),
    ];
    match run_git(&repo_root, &rm_args) {
        Ok(out) if out.status.success() => {}
        Ok(out) if is_unknown_worktree(&out.stderr) => {} // already pruned
        Ok(out) => {
            first_err.get_or_insert(git_err(&rm_args, &out));
        }
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    // (b) Belt-and-suspenders: `worktree remove` refuses ("not a working
    // tree") once the admin metadata is gone, even on the rare path where
    // the checkout directory itself is somehow still present (e.g. a prior
    // partial cleanup removed the metadata but not the directory). Cover
    // that gap directly; tolerate NotFound (directory already gone too).
    match std::fs::remove_dir_all(worktree_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            first_err.get_or_insert(e.into());
        }
    }
    // (c) Delete the wip branch (unchanged — a plain ref delete, not a
    // worktree-admin mutation, so it stays on git2 like the rest of the
    // crate).
    match repo.find_branch(wip_branch, git2::BranchType::Local) {
        Ok(mut b) => {
            if let Err(e) = b.delete() {
                first_err.get_or_insert(Error::Git(e));
            }
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => {}
        Err(e) => {
            first_err.get_or_insert(Error::Git(e));
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl VaultRepo {
    /// Open a fan-out scratch worktree (GWS.9). Creates a wip branch
    /// `wip/<id>` at this repo's current HEAD commit, then creates a git
    /// worktree at `worktree_path` (must be OUTSIDE main's working tree —
    /// git refuses nested worktrees). Returns a [`FanoutWorktree`] whose
    /// `worktree_repo()` is the substrate handle for all txns inside the
    /// fan-out.
    ///
    /// Errors if this branch is unborn (no commit to fork from) or detached.
    #[instrument(
        skip(self),
        fields(id = %id, worktree_path = ?worktree_path),
        name = "git_begin_fanout"
    )]
    pub fn begin_fanout(&self, id: &str, worktree_path: &Path) -> Result<FanoutWorktree<'_>> {
        let info = self.open_fanout_worktree(id, worktree_path)?;
        // Open the worktree as a VaultRepo, sharing the commit-lock registry.
        let worktree_repo = VaultRepo::open_with_locks(worktree_path, self.commit_locks())?;
        Ok(FanoutWorktree {
            main: self,
            worktree_repo,
            info,
        })
    }

    /// Stateless variant of [`Self::begin_fanout`] — does the same work but
    /// returns a [`FanoutInfo`] handle that survives the call boundary
    /// (where `FanoutWorktree<'a>`'s borrow on `&self` does not). The MCP
    /// `begin_transaction` tool uses this so it can return to the agent
    /// between the begin call and the eventual `commit_transaction` /
    /// `abandon_transaction`.
    ///
    /// Same preconditions: branch must be born + not detached.
    #[instrument(
        skip(self),
        fields(id = %id, worktree_path = ?worktree_path),
        name = "git_open_fanout_worktree"
    )]
    pub fn open_fanout_worktree(&self, id: &str, worktree_path: &Path) -> Result<FanoutInfo> {
        let main_branch = self.head_ref()?; // errors if detached
        let parent_tip = self
            .head_oid()
            .ok_or_else(|| Error::other("cannot fan-out from an unborn branch"))?;

        let wip_branch = format!("wip/{id}");
        let worktree_name = format!("wip-{id}");

        // Create the wip branch off main's tip. Unchanged — a plain ref
        // write, not a worktree-admin mutation, so it stays on git2 (see the
        // GX.10 helper block above for why worktree add/remove don't).
        let parent_commit = self.git().find_commit(parent_tip)?;
        self.git().branch(&wip_branch, &parent_commit, false)?;

        // Create the git worktree on the wip branch via the CLI. The branch
        // already exists (just created above), so `<commit-ish>` names it
        // directly — no `-b`/`--detach` needed, matching the old git2 call's
        // `opts.reference(Some(&wip_ref))` semantics (check out an existing
        // ref, don't create one).
        let path_str = worktree_path
            .to_str()
            .ok_or_else(|| Error::other("fan-out worktree path is not valid UTF-8"))?;
        // `--` end-of-options guards a `-`-leading path (see cleanup_inner).
        let add_args = ["worktree", "add", "--", path_str, &wip_branch];
        let out = run_git(&repo_root(self), &add_args)?;
        if !out.status.success() {
            return Err(git_err(&add_args, &out));
        }

        Ok(FanoutInfo {
            wip_branch,
            worktree_name,
            worktree_path: worktree_path.to_path_buf(),
            parent_tip,
            main_branch,
        })
    }

    /// Stateless merge-back. Mirrors [`FanoutWorktree::commit_fanout`] but
    /// takes the info handle instead of consuming a borrowed `FanoutWorktree`.
    /// Holds main's commit lock for the critical section and ALWAYS attempts
    /// cleanup (worktree + wip branch) — even on merge error.
    #[instrument(
        skip(self, info),
        fields(
            wip_branch = %info.wip_branch,
            main_branch = %info.main_branch,
            strategy = ?strategy,
        ),
        name = "git_merge_fanout_back"
    )]
    pub fn merge_fanout_back(
        &self,
        info: &FanoutInfo,
        strategy: MergeStrategy,
        message: Option<&str>,
    ) -> Result<MergeBackResult> {
        let result = self.with_commit_lock(|| merge_inner(self, info, strategy, message));
        let _ = cleanup_inner(self, &info.wip_branch, &info.worktree_path);
        result
    }

    /// Stateless abandon — cleanup the worktree + wip branch without
    /// touching main.
    #[instrument(
        skip(self, info),
        fields(
            wip_branch = %info.wip_branch,
            worktree_name = %info.worktree_name,
        ),
        name = "git_abandon_fanout_by_info"
    )]
    pub fn abandon_fanout_by_info(&self, info: &FanoutInfo) -> Result<()> {
        cleanup_inner(self, &info.wip_branch, &info.worktree_path)
    }

    /// Scan this repo's registered worktrees for ones checked out on a
    /// `wip/*` branch — fanout artifacts left over from a previous session.
    /// Pure read; never mutates. Identifies fanout worktrees by their
    /// **branch**, not by worktree admin name (see the GX.10 helper-block
    /// doc above for why the name isn't a reliable signal once worktree
    /// creation goes through the plain `git worktree add` CLI). Caller
    /// decides whether to clean each one up (via
    /// [`Self::abandon_fanout_by_info`] if they can rebuild the [`FanoutInfo`], or
    /// manually via `git worktree remove` + `git branch -D`).
    pub fn list_orphan_fanouts(&self) -> Result<Vec<OrphanFanout>> {
        let entries = list_worktrees(&repo_root(self))?;
        let mut out = Vec::new();
        for entry in entries {
            let Some(branch) = entry.branch.as_deref() else {
                continue; // detached checkout, never a fanout worktree
            };
            let Some(id) = branch.strip_prefix("refs/heads/wip/") else {
                continue;
            };
            out.push(OrphanFanout {
                worktree_name: format!("wip-{id}"),
                wip_branch: format!("wip/{id}"),
                worktree_path: entry.path,
            });
        }
        Ok(out)
    }
}

/// One fan-out artifact (`wip-<id>` worktree + `wip/<id>` branch) found on
/// disk by [`VaultRepo::list_orphan_fanouts`]. Whether a given entry is
/// truly "orphan" — i.e. not tracked by a live caller — is a server-layer
/// concern; the substrate just enumerates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanFanout {
    pub worktree_name: String,
    pub wip_branch: String,
    pub worktree_path: PathBuf,
}

/// Implementation extracted from the old `FanoutWorktree::merge_back` so
/// the stateless and borrowed APIs share one body.
fn merge_inner(
    main: &VaultRepo,
    info: &FanoutInfo,
    strategy: MergeStrategy,
    message: Option<&str>,
) -> Result<MergeBackResult> {
    let repo = main.git();
    let wip_ref = format!("refs/heads/{}", info.wip_branch);

    let wip_tip = repo
        .refname_to_id(&wip_ref)
        .map_err(|e| Error::other(format!("wip branch {} missing: {e}", info.wip_branch)))?;
    let main_tip_before = repo
        .refname_to_id(&info.main_branch)
        .map_err(|e| Error::other(format!("main branch {} missing: {e}", info.main_branch)))?;

    // If the fan-out made no commits, the wip branch still points at the
    // parent tip — there is nothing to merge back. Treat as a no-op success.
    if wip_tip == info.parent_tip {
        return Ok(MergeBackResult {
            tip_after: main_tip_before,
            tip_before: main_tip_before,
            merge_commit: None,
        });
    }

    match strategy {
        MergeStrategy::FastForward => {
            if main_tip_before != info.parent_tip {
                return Err(Error::other(format!(
                    "fast-forward merge-back failed: main advanced ({} -> {}) during the \
                     fan-out; use MergeCommit instead",
                    info.parent_tip, main_tip_before
                )));
            }
            main.cas_ref(&info.main_branch, Some(main_tip_before), wip_tip)?;
            let changed = main.paths_changed_between(main_tip_before, wip_tip)?;
            main.materialize(wip_tip, &changed)?;
            Ok(MergeBackResult {
                tip_after: wip_tip,
                tip_before: main_tip_before,
                merge_commit: None,
            })
        }
        MergeStrategy::MergeCommit => {
            let base_tree = repo.find_commit(info.parent_tip)?.tree()?;
            let ours_tree = repo.find_commit(main_tip_before)?.tree()?;
            let theirs_tree = repo.find_commit(wip_tip)?.tree()?;
            let mut idx = repo.merge_trees(&base_tree, &ours_tree, &theirs_tree, None)?;
            if idx.has_conflicts() {
                return Err(Error::other(format!(
                    "merge-back conflict between main ({}) and wip {} ({}); \
                     resolve manually",
                    main_tip_before, info.wip_branch, wip_tip
                )));
            }
            let merged_tree_oid = idx.write_tree_to(repo)?;
            // turbovault-b1q: caller-supplied merge-commit subject (the fanout
            // merge-back is a mutation that produces a commit); fall back to the
            // auto-derived message when none is given.
            let message = message.map(str::to_string).unwrap_or_else(|| {
                format!(
                    "merge fan-out {} into {}",
                    info.wip_branch, info.main_branch
                )
            });
            let merge_commit_oid =
                main.commit_tree(merged_tree_oid, &[main_tip_before, wip_tip], &message)?;
            main.cas_ref(&info.main_branch, Some(main_tip_before), merge_commit_oid)?;
            let changed = main.paths_changed_between(main_tip_before, merge_commit_oid)?;
            main.materialize(merge_commit_oid, &changed)?;
            Ok(MergeBackResult {
                tip_after: merge_commit_oid,
                tip_before: main_tip_before,
                merge_commit: Some(merge_commit_oid),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use tempfile::TempDir;
    use turbovault_core::ChangePlan;

    /// Init main repo + apply one seed commit so main is BORN (begin_fanout
    /// requires a tip to fork from).
    fn open_born() -> (TempDir, TempDir, VaultRepo) {
        let main_dir = TempDir::new().unwrap();
        // Worktree path must live OUTSIDE main's workdir. Hold its TempDir in
        // its parent so it survives until both are dropped.
        let scratch_parent = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(main_dir.path(), &opts).unwrap();
        let vr = VaultRepo::open(main_dir.path()).unwrap();
        vr.commit_changeset(&ChangePlan::new("seed").create("seed.md", "S"))
            .unwrap();
        (main_dir, scratch_parent, vr)
    }

    fn scratch_path(parent: &TempDir, id: &str) -> PathBuf {
        parent.path().join(format!("worktree-{id}"))
    }

    fn wt_read(repo: &VaultRepo, rel: &str) -> String {
        std::fs::read_to_string(repo.git().workdir().unwrap().join(rel)).unwrap()
    }

    #[test]
    fn begin_isolates_worktree_main_untouched() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "1");
        let fanout = vr.begin_fanout("1", &wt_path).unwrap();

        // The wip branch was created off main's tip.
        let main_tip = vr.head_oid().unwrap();
        assert_eq!(fanout.parent_tip(), main_tip);
        assert_eq!(fanout.wip_branch(), "wip/1");

        // Apply a txn in the fan-out — main's tip is UNCHANGED.
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();
        assert_eq!(
            vr.head_oid(),
            Some(main_tip),
            "main unchanged during fan-out"
        );
        // The worktree's working tree has the new file; main's does not.
        assert_eq!(wt_read(fanout.worktree_repo(), "a.md"), "alpha");
        assert!(!vr.git().workdir().unwrap().join("a.md").exists());

        fanout.abandon_fanout().unwrap();
    }

    #[test]
    fn commit_fanout_merge_commit_lands_on_main_with_two_parents() {
        let (_m, scratch, vr) = open_born();
        let main_tip_before = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "2");
        let fanout = vr.begin_fanout("2", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();

        let merge_oid = res.merge_commit.expect("merge commit expected");
        assert_eq!(vr.head_oid(), Some(merge_oid));
        let merge_commit = vr.git().find_commit(merge_oid).unwrap();
        assert_eq!(
            merge_commit.parent_count(),
            2,
            "merge commit has two parents"
        );
        assert_eq!(merge_commit.parent_id(0).unwrap(), main_tip_before);
        // Main's working tree now contains the fan-out's file.
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
        // Scratch worktree + wip branch cleaned up.
        assert!(!wt_path.exists());
        assert!(
            vr.git()
                .find_branch("wip/2", git2::BranchType::Local)
                .is_err()
        );
    }

    #[test]
    fn commit_fanout_fast_forward_when_main_unchanged() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "3");
        let fanout = vr.begin_fanout("3", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::FastForward).unwrap();
        assert!(res.merge_commit.is_none(), "FF makes no new commit object");
        assert_eq!(vr.head_oid(), Some(res.tip_after));
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
    }

    #[test]
    fn fast_forward_fails_when_main_advanced_concurrently() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "4");
        let fanout = vr.begin_fanout("4", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();

        // Concurrent writer on main (e.g. cross-process / Workflow B) advances
        // main while the fan-out was working.
        vr.commit_changeset(&ChangePlan::new("concurrent").create("c.md", "concurrent"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::FastForward);
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::Other(_)))),
            "FF must refuse when main advanced"
        );
    }

    #[test]
    fn merge_commit_handles_concurrent_main_advance_disjoint() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "5");
        let fanout = vr.begin_fanout("5", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();

        // Concurrent main writer touches a DISJOINT path.
        vr.commit_changeset(&ChangePlan::new("concurrent").create("c.md", "concurrent"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();
        let merge_oid = res.merge_commit.unwrap();
        let tree = vr.git().find_commit(merge_oid).unwrap().tree_id();
        // Both changes present in the merged tree.
        assert!(vr.blob_oid_at(tree, "a.md").unwrap().is_some());
        assert!(vr.blob_oid_at(tree, "c.md").unwrap().is_some());
        // Working tree matches.
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
        assert_eq!(wt_read(&vr, "c.md"), "concurrent");
    }

    /// turbovault-uag: a CONFLICTING same-path edit on main vs. the fanout
    /// worktree must abort the merge-back loudly and leave main untouched —
    /// never a silent 3-way text merge. Every prior merge test used disjoint
    /// paths, so this conflict branch was unverified.
    #[test]
    fn merge_commit_aborts_on_conflicting_same_path_edit() {
        let (_m, scratch, vr) = open_born();
        // Seed a shared file on main so both sides edit the SAME path.
        vr.commit_changeset(&ChangePlan::new("seed").create("shared.md", "base"))
            .unwrap();
        let base = crate::VaultRepo::blob_oid_of(b"base").unwrap();

        let wt_path = scratch_path(&scratch, "conflict");
        let fanout = vr.begin_fanout("conflict", &wt_path).unwrap();
        // wip edits shared.md one way...
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("wip").update(
                "shared.md",
                "wip-side",
                base.to_string(),
            ))
            .unwrap();
        // ...main edits the SAME path a different way (concurrent).
        vr.commit_changeset(&ChangePlan::new("concurrent").update(
            "shared.md",
            "main-side",
            base.to_string(),
        ))
        .unwrap();
        let main_after_concurrent = vr.head_oid().unwrap();

        // Merge-back must ABORT — no silent text merge.
        let res = fanout.commit_fanout(MergeStrategy::MergeCommit);
        assert!(
            res.is_err(),
            "conflicting same-path edit must abort: {res:?}"
        );
        assert!(
            res.unwrap_err().to_string().contains("conflict"),
            "loud conflict error"
        );
        // Main never advanced past the concurrent edit (no merge landed).
        assert_eq!(
            vr.head_oid(),
            Some(main_after_concurrent),
            "main untouched by the aborted merge"
        );
    }

    #[test]
    fn abandon_leaves_main_untouched_and_cleans_up() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "6");
        let fanout = vr.begin_fanout("6", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();

        fanout.abandon_fanout().unwrap();

        assert_eq!(vr.head_oid(), Some(main_tip), "main unchanged on abandon");
        assert!(!wt_path.exists(), "worktree dir removed");
        assert!(
            vr.git()
                .find_branch("wip/6", git2::BranchType::Local)
                .is_err()
        );
    }

    #[test]
    fn empty_fanout_commit_is_a_noop() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "7");
        let fanout = vr.begin_fanout("7", &wt_path).unwrap();
        // No txns applied — wip tip == parent_tip.
        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();
        assert!(res.merge_commit.is_none());
        assert_eq!(res.tip_after, main_tip);
    }

    // -------- GWS.13 stateless fanout API --------

    #[test]
    fn stateless_open_returns_info_borrow_ends() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "stateless-1");
        let info = vr.open_fanout_worktree("stateless-1", &wt_path).unwrap();
        assert_eq!(info.wip_branch, "wip/stateless-1");
        assert_eq!(info.worktree_name, "wip-stateless-1");
        assert_eq!(info.worktree_path, wt_path);
        assert_eq!(info.parent_tip, vr.head_oid().unwrap());

        // info survives — we can now drop it / move it / pass through MCP.
        let info_clone = info.clone();
        vr.abandon_fanout_by_info(&info_clone).unwrap();
        assert!(!wt_path.exists());
    }

    #[test]
    fn stateless_open_then_write_then_merge_back_lands_on_main() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "stateless-2");
        let info = vr.open_fanout_worktree("stateless-2", &wt_path).unwrap();

        // Open the worktree separately (the stateless API doesn't return a
        // VaultRepo handle — caller manages that lifecycle).
        let wt = VaultRepo::open_with_locks(&wt_path, vr.commit_locks()).unwrap();
        wt.commit_changeset(&ChangePlan::new("c").create("page.md", "PAGE"))
            .unwrap();

        let res = vr
            .merge_fanout_back(&info, MergeStrategy::MergeCommit, None)
            .unwrap();
        assert!(res.merge_commit.is_some(), "merge commit landed");
        assert_eq!(wt_read(&vr, "page.md"), "PAGE");
        assert!(!wt_path.exists(), "scratch worktree cleaned up");
    }

    /// turbovault-b1q: a caller-supplied merge message becomes the merge-commit
    /// subject; `None` falls back to the auto-derived message.
    #[test]
    fn merge_fanout_back_uses_caller_supplied_message() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "msg-1");
        let info = vr.open_fanout_worktree("msg-1", &wt_path).unwrap();
        let wt = VaultRepo::open_with_locks(&wt_path, vr.commit_locks()).unwrap();
        wt.commit_changeset(&ChangePlan::new("c").create("page.md", "PAGE"))
            .unwrap();

        let res = vr
            .merge_fanout_back(&info, MergeStrategy::MergeCommit, Some("ingest source X"))
            .unwrap();
        let oid = res.merge_commit.expect("merge commit");
        let msg = vr
            .git()
            .find_commit(oid)
            .unwrap()
            .message()
            .unwrap()
            .to_string();
        assert_eq!(
            msg, "ingest source X",
            "caller message is the merge subject"
        );
    }

    #[test]
    fn stateless_abandon_after_writes_leaves_main_untouched() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "stateless-3");
        let info = vr.open_fanout_worktree("stateless-3", &wt_path).unwrap();

        let wt = VaultRepo::open_with_locks(&wt_path, vr.commit_locks()).unwrap();
        wt.commit_changeset(&ChangePlan::new("c").create("orphan.md", "discarded"))
            .unwrap();

        vr.abandon_fanout_by_info(&info).unwrap();
        assert_eq!(vr.head_oid(), Some(main_tip), "main unchanged");
        assert!(!wt_path.exists());
        assert!(
            vr.git()
                .find_branch("wip/stateless-3", git2::BranchType::Local)
                .is_err()
        );
    }

    #[test]
    fn stateless_merge_back_no_commits_is_noop() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "stateless-4");
        let info = vr.open_fanout_worktree("stateless-4", &wt_path).unwrap();
        // No writes through wt — merge_back should be a no-op.
        let res = vr
            .merge_fanout_back(&info, MergeStrategy::MergeCommit, None)
            .unwrap();
        assert!(res.merge_commit.is_none());
        assert_eq!(res.tip_after, main_tip);
    }

    #[test]
    fn list_orphan_fanouts_empty_when_no_worktrees() {
        let (_m, _scratch, vr) = open_born();
        assert!(vr.list_orphan_fanouts().unwrap().is_empty());
    }

    #[test]
    fn list_orphan_fanouts_detects_open_wip_worktree() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "orphan-1");
        let info = vr.open_fanout_worktree("orphan-1", &wt_path).unwrap();
        let orphans = vr.list_orphan_fanouts().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].worktree_name, "wip-orphan-1");
        assert_eq!(orphans[0].wip_branch, "wip/orphan-1");
        // git2 may canonicalize paths; compare by canonical form.
        assert_eq!(
            orphans[0].worktree_path.canonicalize().unwrap(),
            wt_path.canonicalize().unwrap()
        );
        // Cleanup so the temp dirs drop cleanly.
        vr.abandon_fanout_by_info(&info).unwrap();
    }

    #[test]
    fn list_orphan_fanouts_skips_non_wip_worktrees() {
        let (_m, scratch, vr) = open_born();
        // Create a worktree on a NEW branch (not main, since main is
        // checked out in the primary worktree). Name it without the `wip-`
        // prefix to verify the filter.
        let wt_path = scratch.path().join("worktree-other");
        let head_oid = vr.head_oid().unwrap();
        let head_commit = vr.git().find_commit(head_oid).unwrap();
        let feature_branch = vr.git().branch("feature-x", &head_commit, false).unwrap();
        let feature_ref = feature_branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&feature_ref));
        let _wt = vr
            .git()
            .worktree("notwip-1", &wt_path, Some(&opts))
            .unwrap();
        let orphans = vr.list_orphan_fanouts().unwrap();
        assert!(
            orphans.is_empty(),
            "non-wip worktree should not be reported, got: {:?}",
            orphans
        );
    }

    #[test]
    fn list_orphan_fanouts_detects_after_abandon_is_empty() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "orphan-2");
        let info = vr.open_fanout_worktree("orphan-2", &wt_path).unwrap();
        assert_eq!(vr.list_orphan_fanouts().unwrap().len(), 1);
        vr.abandon_fanout_by_info(&info).unwrap();
        assert!(
            vr.list_orphan_fanouts().unwrap().is_empty(),
            "abandon should remove the orphan entry"
        );
    }
}
