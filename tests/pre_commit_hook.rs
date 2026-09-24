//! Guards on the `.githooks/pre-commit` gate.
//!
//! The hook is the only thing standing between a red tree and a commit, and it
//! reports a single verdict — "All checks passed." — so a gap in what it runs
//! is invisible at exactly the moment it matters. It ran `cargo test` and
//! `cargo clippy` without `--workspace` for its whole life. The root package is
//! `dbd-cli`, deliberately *not* a workspace member (see the note in
//! `Cargo.toml`), so an unscoped cargo command compiles the CLI and nothing
//! else: `dbd-core` — the parser, the differ, the adapters, and 977 of the
//! ~1200 tests — was never built. The hook printed "All checks passed." over a
//! workspace that `cargo test --workspace` failed with two red parser tests.
//!
//! CI (`.github/workflows/ci.yml`) and `make bump` both pass `--workspace`, so
//! a release was never at risk. The local loop was: a broken parser could be
//! committed, and only a push would reveal it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn hook_text() -> String {
    let path = root().join(".githooks/pre-commit");
    assert!(
        path.exists(),
        "the pre-commit hook is missing — it is installed with \
         `git config core.hooksPath .githooks`, so the file must exist in-tree"
    );
    fs::read_to_string(&path).expect("pre-commit hook must be readable")
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

/// Every command the hook ultimately runs: its own lines, plus the expansion of
/// any make target it delegates to. Resolving the delegation is what lets this
/// test the *property* rather than one spelling of it — the hook passes whether
/// it names the cargo flags itself or defers to the Makefile, and fails either
/// way if the scope is wrong.
fn effective_commands() -> Vec<String> {
    let hook = hook_text();
    let mut lines: Vec<String> = hook.lines().map(str::to_string).collect();

    for target in ["_check-ci"] {
        if hook.contains(&format!("make {target}")) {
            lines.extend(make_dry_run(target).lines().map(str::to_string));
        }
    }
    lines
}

/// A cargo invocation that compiles or checks code, and the scope it was given.
///
/// Matched at the start of the line, after stripping make's `@` prefix — not
/// anywhere in it. `echo "Running cargo test..."` contains the verb and runs
/// nothing, and a matcher that flagged it would fire on a correct hook.
fn scope_sensitive_cargo(line: &str) -> Option<&str> {
    let t = line.trim().trim_start_matches('@').trim_start();
    // `fmt` is excluded deliberately: it has no `--workspace`, it takes
    // `--all`, and the hook already passes it.
    ["cargo test", "cargo clippy", "cargo build", "cargo check"]
        .into_iter()
        .find(|verb| t.starts_with(verb))
}

/// The whole point of the hook. An unscoped cargo command in this repo does not
/// mean "a bit less coverage" — it means `dbd-core` is not compiled at all.
#[test]
fn pre_commit_gates_the_whole_workspace() {
    let unscoped: Vec<String> = effective_commands()
        .into_iter()
        .filter(|l| scope_sensitive_cargo(l).is_some())
        .filter(|l| !l.contains("--workspace") && !l.contains("-p "))
        .collect();

    assert!(
        unscoped.is_empty(),
        "the pre-commit hook runs cargo without workspace scope, so `dbd-core` \
         is never compiled and its tests never run:\n{}\n\n\
         The root package is `dbd-cli` and is not a workspace member, so a bare \
         `cargo test` covers the CLI only.",
        unscoped.join("\n")
    );
}

/// The hook and the release pre-flight must not be two hand-maintained copies
/// of the same list. They were, and they drifted: `_check-ci` gained
/// `--workspace` and the hook did not. One definition, so the next flag added
/// to the release gate reaches the commit gate for free.
#[test]
fn pre_commit_does_not_restate_the_release_preflight() {
    let hook = hook_text();
    assert!(
        hook.contains("make _check-ci"),
        "the hook must delegate to the Makefile's `_check-ci` rather than \
         restating its commands — that duplication is what let the two drift"
    );

    let own_cargo: Vec<&str> = hook
        .lines()
        .filter(|l| scope_sensitive_cargo(l).is_some())
        .collect();
    assert!(
        own_cargo.is_empty(),
        "the hook still names cargo commands of its own, which can drift from \
         `_check-ci` again:\n{}",
        own_cargo.join("\n")
    );
}

/// A hook that is not executable is not a gate at all — git skips it silently,
/// and every commit passes.
#[test]
fn pre_commit_hook_is_executable() {
    let path = root().join(".githooks/pre-commit");
    assert!(is_executable(&path), "{} must be executable", path.display());
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}
