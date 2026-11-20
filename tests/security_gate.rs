//! Guards on the security gate (`make _check-security`).
//!
//! `qlty check` reports security findings but always exits 0 — even with high
//! findings present, even with `--fail-level low` — so it cannot gate
//! anything. The gate therefore runs `zizmor` directly: it exits nonzero on
//! any finding in its default persona and needs no network (offline audits
//! only), so the verdict is deterministic in every layer that runs it.
//!
//! The layers, all running the one Makefile definition: the pre-commit hook
//! (via `_check-ci`, so before the commit exists), CI (before merge), `make
//! bump` (via `_check-ci`, before the tag), and `release.yml` (before the
//! irreversible publish). A finding must fail in all four, which is what the
//! wiring tests below pin: the definition, its inclusion in `_check-ci`, and
//! its presence in both workflows — with the release gate positioned before
//! the publish step, since a gate after the upload is no gate at all.
//!
//! The zizmor version is pinned to a minor (`1.29` floats patch releases)
//! and must read identically everywhere it appears, or local and CI can
//! disagree about what is clean.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Expand a make target without running it. Safe only for targets whose recipe
/// contains no `$(MAKE)`: make executes those even under `-n`.
fn make_dry_run(target: &str) -> String {
    let out = Command::new("make")
        .args(["-n", target])
        .current_dir(root())
        .output()
        .expect("make must be on PATH");
    assert!(
        out.status.success(),
        "`make -n {target}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn makefile_text() -> String {
    fs::read_to_string(root().join("Makefile")).expect("Makefile must be readable")
}

/// The gate exists and audits with zizmor.
#[test]
fn security_gate_runs_zizmor() {
    let expansion = make_dry_run("_check-security");
    assert!(
        expansion.contains("zizmor"),
        "`_check-security` does not run zizmor:\n{expansion}"
    );
}

/// The commit gate and `make bump` both run `_check-ci`, so the security gate
/// must be part of it — otherwise a finding passes the commit and fails on push.
#[test]
fn commit_gate_includes_the_security_gate() {
    let expansion = make_dry_run("_check-ci");
    assert!(
        expansion.contains("zizmor"),
        "`_check-ci` does not run the security gate, so a finding the hook \
         passed fails on push:\n{expansion}"
    );
}

/// CI must run the same definition, not a restatement of it.
#[test]
fn ci_runs_the_security_gate() {
    let ci = fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("ci.yml must be readable");
    assert!(
        ci.contains("make _check-security"),
        "ci.yml does not run `make _check-security` — the push gate is missing"
    );
}

/// The release workflow must run the gate before the publish step. Tags do not
/// trigger CI, so this step is the only thing between a finding and crates.io.
#[test]
fn release_gates_publish_on_security() {
    let release =
        fs::read_to_string(root().join(".github/workflows/release.yml")).expect("release.yml must be readable");
    let gate = release.find("make _check-security");
    let publish = release.find("cargo publish");
    assert!(
        gate.is_some(),
        "release.yml does not run `make _check-security` — the release gate is missing"
    );
    assert!(
        publish.is_some_and(|p| gate.is_some_and(|g| g < p)),
        "the security gate must come before `cargo publish` in release.yml — \
         a gate after the upload is no gate at all"
    );
}

/// The pinned zizmor minor must read identically in the Makefile and in both
/// workflow installs, or local and CI can disagree about what is clean.
#[test]
fn zizmor_version_is_pinned_consistently() {
    let makefile = makefile_text();
    let pinned = makefile
        .lines()
        .find_map(|l| l.trim().strip_prefix("ZIZMOR_VERSION :=").map(|v| v.trim().to_string()));
    let pinned = pinned.expect("Makefile must define ZIZMOR_VERSION");

    let mut installs = Vec::new();
    for workflow in ["ci.yml", "release.yml"] {
        let text =
            fs::read_to_string(root().join(".github/workflows").join(workflow)).expect("workflow must be readable");
        for line in text.lines() {
            if let Some(idx) = line.find("zizmor@") {
                let version: String = line[idx + "zizmor@".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                installs.push((workflow, version));
            }
        }
    }
    assert_eq!(
        installs.len(),
        2,
        "expected one zizmor install in each workflow, found: {installs:?}"
    );
    for (workflow, version) in &installs {
        assert_eq!(
            *version, pinned,
            "{workflow} installs zizmor@{version} but ZIZMOR_VERSION is {pinned}"
        );
    }
}
