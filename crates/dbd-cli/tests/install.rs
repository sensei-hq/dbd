//! End-to-end tests for `dbd install` — drives the real binary and asserts the
//! embedded skill/agent land at the right paths, honouring `--project`,
//! `--dry-run`, and the `$CLAUDE_CONFIG_DIR` global override.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

/// Canonical embedded content, read from the source tree so the test asserts the
/// *installed bytes* match what ships (not just that some file appeared).
fn canonical_skill() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/skills/dbd/SKILL.md")).unwrap()
}
fn canonical_agent() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/agents/dbd-pattern-verifier.md")).unwrap()
}

/// Global install writes both assets under `$CLAUDE_CONFIG_DIR`, byte-for-byte.
#[test]
fn global_install_writes_assets_under_config_dir() {
    let home = tempfile::tempdir().unwrap();
    let base = home.path();

    Command::cargo_bin("dbd")
        .unwrap()
        .arg("install")
        .env("CLAUDE_CONFIG_DIR", base)
        .assert()
        .success()
        .stdout(contains("created"));

    let skill = base.join("skills/dbd/SKILL.md");
    let agent = base.join("agents/dbd-pattern-verifier.md");
    assert_eq!(fs::read_to_string(&skill).unwrap(), canonical_skill());
    assert_eq!(fs::read_to_string(&agent).unwrap(), canonical_agent());
}

/// `--dry-run` reports what it would do but writes nothing.
#[test]
fn dry_run_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let base = home.path();

    Command::cargo_bin("dbd")
        .unwrap()
        .args(["install", "--dry-run"])
        .env("CLAUDE_CONFIG_DIR", base)
        .assert()
        .success()
        .stdout(contains("Would install"));

    assert!(!base.join("skills/dbd/SKILL.md").exists(), "dry-run must not write the skill");
    assert!(!base.join("agents/dbd-pattern-verifier.md").exists(), "dry-run must not write the agent");
}

/// `--project` installs into `<cwd>/.claude`, ignoring the global config dir.
#[test]
fn project_install_writes_under_local_dot_claude() {
    let proj = tempfile::tempdir().unwrap();
    // A different, empty global base — it must stay untouched.
    let global = tempfile::tempdir().unwrap();

    Command::cargo_bin("dbd")
        .unwrap()
        .args(["install", "--project"])
        .env("CLAUDE_CONFIG_DIR", global.path())
        .current_dir(proj.path())
        .assert()
        .success();

    assert!(proj.path().join(".claude/skills/dbd/SKILL.md").exists(), "project skill should exist");
    assert!(proj.path().join(".claude/agents/dbd-pattern-verifier.md").exists(), "project agent should exist");
    assert!(!global.path().join("skills").exists(), "--project must not touch the global base");
}

/// Re-running is idempotent: the second run reports every asset unchanged.
#[test]
fn second_run_is_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let base = home.path();

    let mut first = Command::cargo_bin("dbd").unwrap();
    first.arg("install").env("CLAUDE_CONFIG_DIR", base).assert().success();

    Command::cargo_bin("dbd")
        .unwrap()
        .arg("install")
        .env("CLAUDE_CONFIG_DIR", base)
        .assert()
        .success()
        .stdout(contains("2 unchanged"));
}
