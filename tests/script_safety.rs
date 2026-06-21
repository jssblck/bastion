//! Static checks for installer safety behavior.
//!
//! These tests read the installer scripts under `scripts/` as text and assert
//! that the checksum-verification path fails closed: a missing, unparseable, or
//! mismatched checksum must stop the install, never warn and continue. The
//! installers are the release-facing entrypoint, so a regression here would let
//! an unverified binary land on a user's `PATH`.
//!
//! (The test crate is named `script_safety` rather than anything containing
//! "install"/"setup"/"update" on purpose: Windows' UAC installer-detection
//! heuristic forces an elevation prompt on any executable whose name carries one
//! of those tokens, which would make the test binary unrunnable in plain CI.)

use std::{fs, path::Path};

fn script(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn powershell_installer_fails_closed_when_checksum_verification_is_unavailable() {
    let script = script("install.ps1");

    assert!(
        !script.contains("Write-Warning-Message \"Could not verify checksum"),
        "checksum verification must not warn and continue"
    );
    assert!(
        script.contains("Write-Error-Message \"Could not download checksums"),
        "missing checksum downloads must stop installation"
    );
    assert!(
        script.contains("Write-Error-Message \"Could not find checksum"),
        "missing archive checksums must stop installation"
    );
    assert!(
        script.contains("Write-Error-Message \"Checksum verification failed!"),
        "checksum mismatches must stop installation"
    );
}

#[test]
fn shell_installer_fails_closed_when_checksum_verification_is_unavailable() {
    let script = script("install.sh");

    // `set -euo pipefail` makes the installer abort on any unhandled failure
    // instead of limping on with a half-downloaded archive.
    assert!(
        script.contains("set -euo pipefail"),
        "shell installer must run under strict mode"
    );
    assert!(
        script.contains("fail \"Couldn't find checksum for"),
        "missing archive checksums must stop installation"
    );
    assert!(
        script.contains("fail \"Checksum verification failed!"),
        "checksum mismatches must stop installation"
    );
    // The only branch that resolves a checksum mismatch is `fail`; there is no
    // warn-and-continue escape hatch.
    assert!(
        !script.contains("warn \"Checksum"),
        "checksum problems must never be downgraded to a warning"
    );
}
