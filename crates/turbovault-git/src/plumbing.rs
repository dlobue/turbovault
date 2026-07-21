//! Object-DB plumbing (GWS.2): build trees via the **gix tree Editor** and
//! create commit objects, with **no working-tree interaction**.
//!
//! The substrate stages from the batch's own bytes (not the working tree)
//! straight into the object DB: blobs are written with `gix`'s
//! `write_blob`, and a tree Editor — seeded from a parent tree or the empty
//! tree — applies the changeset's changes and writes the resulting tree. The
//! Editor operates on tree objects directly; `.git/index` is never touched.
//! Advancing a ref (CAS) and materializing the working tree are separate,
//! later steps (GWS.3, GWS.5).

use crate::error::{Error, Result};
use crate::oid;
use crate::repo::VaultRepo;
use git2::Oid;
use std::path::Path;
use tracing::instrument;

/// A single change to apply to a tree. Moves are modeled at the op-mapping layer
/// (GWS.8) as `Remove(old)` + `Upsert(new)`.
#[derive(Debug, Clone)]
pub enum TreeChange {
    /// Add a new file or overwrite an existing one with `content`.
    Upsert { path: String, content: Vec<u8> },
    /// Remove a file from the tree.
    Remove { path: String },
}

impl TreeChange {
    /// The vault-relative path this change targets.
    pub fn path(&self) -> &str {
        match self {
            TreeChange::Upsert { path, .. } | TreeChange::Remove { path } => path,
        }
    }
}

impl VaultRepo {
    /// Build a tree from `base` (a parent commit's tree oid, or `None` for an
    /// empty base) applying `changes` via the **gix tree Editor**. Blobs and
    /// the resulting tree are written to the object DB. The shared
    /// `.git/index` is never touched — the Editor edits tree objects
    /// directly. Returns the new tree oid.
    ///
    /// Ported to gix (GX.3): `EntryKind::Blob` is the regular-file mode
    /// (100644), matching the previous git2 staging mode. Blobs are written
    /// before their `upsert`, so `write()`'s existing-oid validation always
    /// holds.
    #[instrument(
        skip(self, changes),
        fields(base = ?base, n_changes = changes.len()),
        name = "git_build_tree"
    )]
    pub fn build_tree(&self, base: Option<Oid>, changes: &[TreeChange]) -> Result<Oid> {
        let repo = self.gix();
        let mut editor = match base {
            Some(base_oid) => repo
                .find_tree(oid::to_gix(base_oid))
                .map_err(|e| Error::other(e.to_string()))?
                .edit()
                .map_err(|e| Error::other(e.to_string()))?,
            None => repo
                .empty_tree()
                .edit()
                .map_err(|e| Error::other(e.to_string()))?,
        };
        for change in changes {
            match change {
                TreeChange::Upsert { path, content } => {
                    let blob_oid = repo
                        .write_blob(content)
                        .map_err(|e| Error::other(e.to_string()))?
                        .detach();
                    editor
                        .upsert(path, gix::objs::tree::EntryKind::Blob, blob_oid)
                        .map_err(|e| Error::other(e.to_string()))?;
                }
                TreeChange::Remove { path } => {
                    // `remove_leaf` (not `remove`) errors if `path` names a
                    // tree rather than a blob, matching the old git2-index
                    // behavior (`git_index_remove_bypath` only matches blob
                    // entries) — a `Remove` must not silently delete an
                    // entire subtree.
                    editor
                        .remove_leaf(path)
                        .map_err(|e| Error::other(e.to_string()))?;
                }
            }
        }
        let written = editor.write().map_err(|e| Error::other(e.to_string()))?;
        Ok(oid::from_gix(written.detach()))
    }

    /// Create a commit object from `tree` and `parents` **without moving any
    /// ref** (this is `commit-tree`, not `commit`). The ref advance is a separate
    /// CAS step (GWS.3). Returns the new commit oid.
    ///
    /// Ported to gix (GX.4): `repo.write_object` writes the commit object
    /// only and takes no `update_ref`-style parameter, so there is no
    /// ref-moving alternative API to accidentally reach for here.
    #[instrument(
        skip(self),
        fields(tree = %tree, n_parents = parents.len(), message = %message),
        name = "git_commit_tree"
    )]
    pub fn commit_tree(&self, tree: Oid, parents: &[Oid], message: &str) -> Result<Oid> {
        let repo = self.gix();
        // gix's `write_object` serializes and hashes the commit bytes without
        // dereferencing `tree`/`parents`, unlike the old git2
        // `find_tree`/`find_commit` calls this replaced — a bad oid would
        // otherwise be written as a dangling commit instead of erroring here.
        repo.find_tree(oid::to_gix(tree))
            .map_err(|e| Error::other(e.to_string()))?;
        for parent in parents {
            repo.find_commit(oid::to_gix(*parent))
                .map_err(|e| Error::other(e.to_string()))?;
        }
        let sig = self.author_signature();
        let commit = gix::objs::Commit {
            tree: oid::to_gix(tree),
            parents: parents.iter().map(|p| oid::to_gix(*p)).collect(),
            author: sig.clone(),
            committer: sig,
            encoding: None,
            message: message.into(),
            extra_headers: vec![],
        };
        let written = repo
            .write_object(&commit)
            .map_err(|e| Error::other(e.to_string()))?;
        Ok(oid::from_gix(written.detach()))
    }

    /// The blob oid at `path` in `tree`, or `None` if absent. This is the value
    /// a changeset reads as its CAS pre-image (GWS.4) and what materialization
    /// resolves to working-tree bytes (GWS.5).
    ///
    /// Ported to gix (GX.2): `lookup_entry_by_path` returns `Ok(None)` for an
    /// absent path directly, unlike git2's NotFound-error-code match.
    pub fn blob_oid_at(&self, tree: Oid, path: &str) -> Result<Option<Oid>> {
        let repo = self.gix();
        let tree = repo
            .find_tree(oid::to_gix(tree))
            .map_err(|e| Error::other(e.to_string()))?;
        let entry = tree
            .lookup_entry_by_path(Path::new(path))
            .map_err(|e| Error::other(e.to_string()))?;
        Ok(entry.map(|e| oid::from_gix(e.id().detach())))
    }

    /// Read a blob's bytes by oid.
    ///
    /// Ported to gix (GX.2): `find_blob` decodes straight to an owned
    /// `Vec<u8>`; `mem::take` lifts it out without a copy (the `Blob`'s
    /// `Drop` impl returns its buffer to gix's reuse pool, so a plain field
    /// move is rejected — swapping the field through `&mut` is not).
    pub fn read_blob(&self, oid: Oid) -> Result<Vec<u8>> {
        let repo = self.gix();
        let mut blob = repo
            .find_blob(oid::to_gix(oid))
            .map_err(|e| Error::other(e.to_string()))?;
        Ok(std::mem::take(&mut blob.data))
    }

    /// Author/committer signature.
    ///
    /// turbovault-ov7 / TV-004: defaults to the built-in
    /// `TurboVault <turbovault@localhost>` identity so machine-authored
    /// commits are visibly distinguishable from human commits in
    /// `git log` / `git blame`. The previous behavior pulled the
    /// operator's global `user.name` / `user.email` first, muddying
    /// the audit trail and blocking "act only on bot commits"
    /// automation.
    ///
    /// Per-vault override via `VaultGitConfig::author` is the
    /// documented upgrade path (architecture §13.5); plumbing that
    /// override into the substrate is a follow-up — until then this
    /// is the single default.
    ///
    /// Ported to gix (GX.4): `gix_actor::Signature` is a plain struct
    /// literal, so unlike the old git2 constructor this can't fail and the
    /// `Result` is dropped.
    fn author_signature(&self) -> gix::actor::Signature {
        gix::actor::Signature {
            name: "TurboVault".into(),
            email: "turbovault@localhost".into(),
            time: gix::date::Time::now_local_or_utc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use tempfile::TempDir;

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

    #[test]
    fn build_tree_from_empty_base() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let oid = vr.blob_oid_at(t, "a.md").unwrap().expect("a.md present");
        assert_eq!(vr.read_blob(oid).unwrap(), b"alpha");
        assert!(vr.blob_oid_at(t, "missing.md").unwrap().is_none());
    }

    #[test]
    fn build_tree_seeds_from_base_and_adds() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let t2 = vr.build_tree(Some(t1), &[upsert("b.md", "beta")]).unwrap();
        // a.md preserved, b.md added.
        assert!(vr.blob_oid_at(t2, "a.md").unwrap().is_some());
        let b = vr.blob_oid_at(t2, "b.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(b).unwrap(), b"beta");
    }

    #[test]
    fn upsert_overwrites_existing() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "v1")]).unwrap();
        let t2 = vr.build_tree(Some(t1), &[upsert("a.md", "v2")]).unwrap();
        let oid = vr.blob_oid_at(t2, "a.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(oid).unwrap(), b"v2");
    }

    #[test]
    fn remove_drops_path() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr
            .build_tree(None, &[upsert("a.md", "alpha"), upsert("b.md", "beta")])
            .unwrap();
        let t2 = vr
            .build_tree(
                Some(t1),
                &[TreeChange::Remove {
                    path: "a.md".to_string(),
                }],
            )
            .unwrap();
        assert!(
            vr.blob_oid_at(t2, "a.md").unwrap().is_none(),
            "a.md removed"
        );
        assert!(vr.blob_oid_at(t2, "b.md").unwrap().is_some(), "b.md kept");
    }

    /// Regression (GX.3): a `Remove` whose path names a **directory** must
    /// error, not silently wipe the subtree. gix `Editor::remove`
    /// (RemoveMode::Any) would delete the whole subtree with no error;
    /// `remove_leaf` (RemoveMode::LeafOnly) errors on a tree, restoring the old
    /// git2-index abort-nothing-applied behavior (`git_index_remove_bypath`
    /// matched only blob entries).
    #[test]
    fn remove_of_a_directory_path_errors_not_a_silent_subtree_wipe() {
        let (_tmp, vr) = open_unborn();
        let base = vr
            .build_tree(
                None,
                &[upsert("dir/note.md", "secret"), upsert("keep.md", "y")],
            )
            .unwrap();
        let res = vr.build_tree(
            Some(base),
            &[TreeChange::Remove {
                path: "dir".to_string(),
            }],
        );
        assert!(
            res.is_err(),
            "removing a directory path must error, not wipe the subtree"
        );
        // build_tree is pure (it builds a NEW tree from base); base is untouched.
        assert!(
            vr.blob_oid_at(base, "dir/note.md").unwrap().is_some(),
            "the base subtree must remain intact after the rejected remove"
        );
    }

    #[test]
    fn commit_tree_creates_object_without_moving_ref() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let c1 = vr.commit_tree(t1, &[], "init").unwrap();

        // The branch is still unborn: commit_tree built an object but moved no ref.
        assert!(vr.is_unborn(), "commit_tree must NOT advance any ref");
        assert_eq!(vr.head_oid(), None);

        // Parent linkage + tree content round-trip.
        let t2 = vr.build_tree(Some(t1), &[upsert("b.md", "beta")]).unwrap();
        let c2 = vr.commit_tree(t2, &[c1], "add b").unwrap();
        let commit2 = vr.git().find_commit(c2).unwrap();
        assert_eq!(commit2.parent_count(), 1);
        assert_eq!(commit2.parent_id(0).unwrap(), c1);
        let b = vr.blob_oid_at(commit2.tree_id(), "b.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(b).unwrap(), b"beta");
    }

    /// Regression (GX.4): `commit_tree` must error on a bogus tree or parent
    /// oid rather than writing a dangling commit — the explicit
    /// `find_tree`/`find_commit` existence checks in `commit_tree` exist
    /// specifically to reject this before `write_object` runs.
    #[test]
    fn commit_tree_rejects_bogus_tree_and_parent_oids() {
        let (_tmp, vr) = open_unborn();
        let bogus = Oid::ZERO_SHA1;

        assert!(
            vr.commit_tree(bogus, &[], "msg").is_err(),
            "commit_tree must error on a nonexistent tree oid"
        );

        let t1 = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        assert!(
            vr.commit_tree(t1, &[bogus], "msg").is_err(),
            "commit_tree must error on a nonexistent parent oid"
        );
    }
}
