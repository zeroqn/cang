//! `/nix` overlay guest environment contribution.
//!
//! `CANG_NIX_OVERLAY` is what makes the guest prepare `/nix` at all. Whether the
//! host grafts its own overlay at `/nix` is a launch decision, not a property of
//! the persistent disks, so `CANG_NIX_HOST_OVERLAY` travels with the launch
//! config instead (`runtime/session/mod.rs`): a launch whose two `/nix` layers are
//! both on the block disk grafts nothing, and the guest composes the only `/nix`
//! there is.
//!
//! Normal cang launches no longer prepare or attach a raw `/nix` disk. The
//! historical file name is retained only so tests can prove the normal path does
//! not create it.

#[cfg(test)]
pub(super) const FILE_NAME: &str = "cang-nix.raw";

pub(super) fn overlay_env_pairs() -> [(String, String); 1] {
    [("CANG_NIX_OVERLAY".to_owned(), "1".to_owned())]
}
