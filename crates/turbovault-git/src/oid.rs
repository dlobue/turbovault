//! Internal git2 <-> gix oid conversion (GX.0 scaffold).
//!
//! `Oid` stays INTERNAL to this crate (design Decision 9 / §11.2): the
//! external write-safety contract is the hex `Precondition::ExpectBlob`
//! token, parsed to an oid ONLY inside the substrate — so neither
//! `git2::Oid` nor `gix::ObjectId` needs a public abstraction over the other,
//! just a same-crate bridge. During the git2 -> gix port (GX.1-GX.10),
//! already-ported gix modules and not-yet-ported git2 modules interoperate
//! through these two shims. Both are `[u8; 20]` SHA-1 digests, so the
//! conversion is a byte copy; `gix::ObjectId` MUST NOT appear in any public
//! signature of this crate.

/// Convert a git2 SHA-1 oid to its gix equivalent.
pub(crate) fn to_gix(oid: git2::Oid) -> gix::ObjectId {
    gix::ObjectId::from_bytes_or_panic(oid.as_bytes())
}

/// Convert a gix SHA-1 object id back to its git2 equivalent (see
/// [`to_gix`]).
pub(crate) fn from_gix(oid: gix::ObjectId) -> git2::Oid {
    git2::Oid::from_bytes(oid.as_bytes()).expect("gix::ObjectId is always a 20-byte SHA-1 digest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_git2_to_gix_to_git2() {
        let original = git2::Oid::hash_object(git2::ObjectType::Blob, b"round-trip").unwrap();
        assert_eq!(from_gix(to_gix(original)), original);
    }

    #[test]
    fn round_trip_gix_to_git2_to_gix() {
        let original =
            gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, b"round-trip")
                .unwrap();
        assert_eq!(to_gix(from_gix(original)), original);
    }

    /// GX.0 D2 / cluster A: the version-token hex of a known blob MUST be
    /// byte-identical whether computed via git2 (`VaultRepo::blob_oid_of`)
    /// or gix (`compute_hash`) — this is the read/write token contract the
    /// whole cutover depends on (a token an agent read via one backend must
    /// still validate against a tree built by the other, mid-port).
    #[test]
    fn blob_hash_is_identical_across_git2_and_gix() {
        let content = b"the quick brown fox jumps over the lazy dog";

        let git2_oid = crate::repo::VaultRepo::blob_oid_of(content).unwrap();
        let gix_oid =
            gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, content).unwrap();

        assert_eq!(
            to_gix(git2_oid),
            gix_oid,
            "git2 and gix must hash the same blob bytes to the same oid"
        );
        assert_eq!(
            git2_oid.to_string(),
            gix_oid.to_string(),
            "the hex version token must match byte-for-byte across backends"
        );
    }
}
