//! Derives the `--version` string at build time.
//!
//! Precedence:
//! 1. `BASTION_VERSION` env var, when set and non-empty (release pipelines).
//! 2. `git describe --always --tags --dirty=-dirty` (tag, else short SHA, with a
//!    `-dirty` suffix when the working tree has uncommitted changes).
//! 3. The crate's `Cargo.toml` version, when git is unavailable (e.g. a source
//!    tarball with no `.git`).

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=BASTION_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    let version = std::env::var("BASTION_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    println!(
        "cargo:rustc-env=BASTION_VERSION={}",
        sanitize_version(&version)
    );
}

fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--always", "--tags", "--dirty=-dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    let version = version.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Keeps the reported version to a predictable, printable character set so a
/// stray ref name can never inject control characters into `--version` output.
fn sanitize_version(raw: &str) -> String {
    let mut version = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if version.is_empty() {
        version = env!("CARGO_PKG_VERSION").to_string();
    }
    version.truncate(128);
    version
}
