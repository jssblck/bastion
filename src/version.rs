//! The version string reported by `bastion --version`.

/// The build-time version, derived from git by `build.rs`.
///
/// This is a release tag when one is reachable, otherwise the short commit SHA,
/// with a `-dirty` suffix when the working tree had uncommitted changes at build
/// time. It falls back to the `Cargo.toml` version when git is unavailable.
pub const VERSION: &str = env!("BASTION_VERSION");
