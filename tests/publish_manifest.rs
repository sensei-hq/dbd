//! Guards that keep the workspace publishable to crates.io.
//!
//! `cargo publish` refuses a dependency that carries only a `path` — the
//! published crate has no way to resolve it. That refusal only ever surfaces at
//! publish time, which is the worst moment to discover it, so it is asserted
//! here against `cargo metadata` (the resolved view, not the manifest text).

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn workspace_metadata() -> Value {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .expect("cargo must be on PATH");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("metadata is JSON")
}

fn package<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no package named {name} in the workspace"))
}

/// `cargo install --path .` must resolve at the repo root.
///
/// pre-commit's `language: rust` runs exactly that from the repo root, so a
/// virtual manifest there (`[workspace]` with no `[package]`) makes the
/// `dbd-format` hook abort with "found a virtual manifest instead of a package
/// manifest" — during environment setup, before it ever formats anything.
#[test]
fn the_repo_root_is_an_installable_package() {
    let meta = workspace_metadata();
    let root_manifest = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .canonicalize()
        .expect("root manifest exists");

    let root_pkg = meta["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|p| {
            Path::new(p["manifest_path"].as_str().unwrap_or_default())
                .canonicalize()
                .map(|m| m == root_manifest)
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("the repo root declares no [package] — `cargo install --path .` cannot resolve it"));

    let has_dbd_bin = root_pkg["targets"]
        .as_array()
        .expect("targets array")
        .iter()
        .any(|t| t["name"] == "dbd" && t["kind"].as_array().is_some_and(|k| k.iter().any(|v| v == "bin")));
    assert!(
        has_dbd_bin,
        "the root package must produce the `dbd` binary that the hook's `entry` invokes"
    );
}

/// The "exhaustive" full reference must actually list every command.
///
/// `docs/skills/dbd/SKILL.md` points readers at `llms-full.txt` as the
/// "exhaustive, always current" reference, and that claim ships embedded in the
/// binary — `dbd install` writes it into every user's config. A completeness
/// claim that is false is worse than no claim: it stops a reader looking
/// further, so a missing command reads as a missing feature.
#[test]
fn the_full_reference_documents_every_command() {
    let help = Command::new(env!("CARGO_BIN_EXE_dbd"))
        .arg("--help")
        .output()
        .expect("the dbd binary must build");
    assert!(help.status.success(), "dbd --help failed");
    let help = String::from_utf8_lossy(&help.stdout);

    let commands: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .take_while(|l| !l.starts_with("Options:"))
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|l| !l.starts_with(' '))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .collect();
    assert!(commands.len() > 15, "parsed too few commands from --help: {commands:?}");

    let reference = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/llms/llms-full.txt"))
        .expect("llms-full.txt");

    // Match the command token, not the whole heading — several carry a suffix
    // (`### dbd release (alias: dbd baseline)`), and an exact-line match would
    // report those as missing.
    let documented: std::collections::HashSet<&str> = reference
        .lines()
        .filter_map(|l| l.strip_prefix("### dbd "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();

    let missing: Vec<&&str> = commands.iter().filter(|c| !documented.contains(*c)).collect();
    assert!(
        missing.is_empty(),
        "llms-full.txt is documented as exhaustive but has no section for: {missing:?}"
    );

    // The README's Commands table is the repo front page's command inventory.
    // A reader treats it as the complete list, so a gap reads as a missing
    // feature and sends them to a worse tool for the job.
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).expect("README.md");
    // A row may carry flags (`dbd migrate --status`), so accept the command
    // followed by a backtick or a space — not the bare backticked form only.
    let is_listed = |c: &str| readme.contains(&format!("`dbd {c}`")) || readme.contains(&format!("`dbd {c} "));
    let listed: Vec<&&str> = commands.iter().filter(|c| is_listed(c)).collect();
    let unlisted: Vec<&&str> = commands.iter().filter(|c| !is_listed(c)).collect();
    assert!(
        unlisted.is_empty(),
        "README lists {} of {} commands; missing: {unlisted:?}",
        listed.len(),
        commands.len()
    );
}

/// A binary crate commits its lockfile.
///
/// Ignoring `Cargo.lock` is the *library* convention — a library's consumers
/// resolve their own versions. This repo's root package is a binary, and both
/// `make install` and the release recipe pass `--locked`, which requires the
/// lockfile to already exist: cargo refuses rather than resolving fresh. So a
/// clone without it cannot run either, and the versions a release is built
/// from stop being recorded anywhere.
#[test]
fn the_lockfile_is_committed() {
    let out = Command::new("git")
        .args(["ls-files", "--error-unmatch", "Cargo.lock"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("git must be on PATH");
    assert!(
        out.status.success(),
        "Cargo.lock is not tracked — `cargo install --path . --locked` fails on a fresh clone: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// Only the crate's own sources may ship to crates.io.
///
/// The package root is the repo root, so everything beside it — `docs/`,
/// `site/`, `crates/dbd-core/`, `tests/fixtures/`, and a `.claude` symlink
/// pointing at the developer's home config — is a candidate for upload unless
/// `include` keeps it out. `include` is gitignore-style, so an unanchored
/// `README.md` matches at *any* depth: that one missing `/` packaged 677 files.
/// A publish is public and cannot be deleted, only yanked.
#[test]
fn only_the_crates_own_sources_are_packaged() {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("cargo")
        .args(["package", "--list", "-p", "dbd-cli", "--allow-dirty"])
        .current_dir(root)
        .output()
        .expect("cargo must be on PATH");
    assert!(
        out.status.success(),
        "cargo package --list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Cargo synthesises these into every package regardless of `include`.
    const GENERATED: [&str; 6] = [
        ".cargo_vcs_info.json",
        "Cargo.toml",
        "Cargo.toml.orig",
        "Cargo.lock",
        "README.md",
        "LICENSE",
    ];

    let listed = String::from_utf8_lossy(&out.stdout);
    let stray: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("src/") && !GENERATED.contains(l))
        .collect();

    assert!(
        stray.is_empty(),
        "these would be published to crates.io and cannot be un-published:\n  {}",
        stray.join("\n  ")
    );
}

/// A path-only dependency cannot be published: `cargo publish` strips the
/// `path` and is left with nothing to resolve against. It must carry a version
/// requirement too.
#[test]
fn the_core_dependency_is_publishable() {
    let meta = workspace_metadata();
    let dep = package(&meta, "dbd-cli")["dependencies"]
        .as_array()
        .expect("dependencies array")
        .iter()
        .find(|d| d["name"] == "dbd-core")
        .expect("dbd-cli depends on dbd-core");

    let req = dep["req"].as_str().expect("a version requirement");
    assert_ne!(
        req, "*",
        "dbd-core is a path-only dependency; `cargo publish -p dbd-cli` will refuse it"
    );
}

/// The requirement must track the version actually in the tree. A stale pin
/// publishes a `dbd-cli` that resolves an older `dbd-core` off crates.io than
/// the one it was built and tested against.
#[test]
fn the_core_dependency_tracks_the_workspace_version() {
    let meta = workspace_metadata();
    let core_version = package(&meta, "dbd-core")["version"]
        .as_str()
        .expect("dbd-core has a version")
        .to_string();

    let dep = package(&meta, "dbd-cli")["dependencies"]
        .as_array()
        .expect("dependencies array")
        .iter()
        .find(|d| d["name"] == "dbd-core")
        .expect("dbd-cli depends on dbd-core");
    let req = dep["req"].as_str().expect("a version requirement");

    assert_eq!(
        req,
        format!("^{core_version}"),
        "the pin drifted from the workspace version ({core_version}) — `make bump` must move both"
    );
}
