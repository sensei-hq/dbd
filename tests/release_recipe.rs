//! Guards on the `make bump` release recipe.
//!
//! The recipe pushes a branch and a tag — irreversible steps — so a failure
//! before them must stop it, and a failure after them must not strand the tree.
//! Both are properties of the emitted recipe, so they are asserted against
//! `make -n bump`, which expands the real rules without running them.

use std::fs;
use std::path::Path;
use std::process::Command;

fn make(args: &[&str]) -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("make")
        .args(args)
        .current_dir(root)
        .output()
        .expect("make must be on PATH");
    assert!(
        out.status.success(),
        "`make {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn bump_recipe() -> String {
    make(&["-n", "bump"])
}

/// Every `origin/<ref>` named in some make output, normalised.
fn origin_refs(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|w| w.split("origin/").nth(1))
        .map(|r| {
            r.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '>')
                .to_string()
        })
        .filter(|r| !r.is_empty())
        .collect()
}

/// Every file the recipe rewrites must reach the release commit.
///
/// `cargo build -q` regenerates Cargo.lock the moment the version seds land, so
/// a `git add` list that omits it tags a lockfile one release behind its
/// Cargo.toml. `cargo install --path . --locked` then fails on any clone of
/// that tag, and the maintainer's own tree is left dirty — which `_check-clean`
/// refuses on the next bump.
#[test]
fn bump_commits_every_file_it_rewrites() {
    let recipe = bump_recipe();
    let commands = logical_commands(&recipe);

    let add = commands
        .iter()
        .find(|c| c.contains("git add"))
        .expect("bump must stage the files it rewrote");

    // Everything a `sed -i` targets, plus the lockfile the build regenerates.
    // A token counts as a target only if it names a file that exists — the sed
    // expressions themselves contain dots and quotes and would otherwise match.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rewritten: Vec<String> = commands
        .iter()
        .filter(|c| c.contains("sed -i"))
        .flat_map(|c| c.split_whitespace().map(str::to_string))
        .filter(|w| root.join(w).is_file())
        .collect();
    rewritten.push("Cargo.lock".to_string());
    rewritten.sort();
    rewritten.dedup();

    for f in &rewritten {
        assert!(
            add.contains(f.as_str()),
            "bump rewrites {f} but never stages it — the tag would ship a stale copy:\n  {add}"
        );
    }
}

/// The bump's `rev:` sed must not rewrite a minimum-version statement.
///
/// That sed matches any `rev: vX.Y.Z`, so a doc sentence written as
/// "needs `rev: v0.12.3` or later" gets silently bumped to the new version on
/// every release — turning a fixed floor into a claim that only ever names the
/// current tag. Minimum versions must therefore be written without the `rev: `
/// prefix. Only the example blocks may carry it.
#[test]
fn the_rev_sed_only_touches_example_blocks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sed_targets = ["README.md", "docs/guide/04-commands.md", "docs/llms/llms-full.txt"];

    for rel in sed_targets {
        let text = fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for (n, line) in text.lines().enumerate() {
            if !line.contains("rev: v") {
                continue;
            }
            // An example block's rev line is just the pin, indented in YAML.
            assert!(
                line.trim_start().starts_with("rev: v"),
                "{rel}:{} states a version inline with `rev: v`, which the bump sed will \
                 rewrite every release:\n  {line}",
                n + 1
            );
        }
    }
}

/// The `dbd-core` pin rewrite must still match what it rewrites.
///
/// That sed matches one exact literal and `sed` exits 0 on no match, so any
/// reformat of the Cargo.toml line — spacing, key order, moving `features`
/// inline — makes it silently no-op. `make bump` then tags and pushes a
/// release whose `dbd-core` pin resolves the *previous* version off crates.io.
/// The guard that would notice, `the_core_dependency_tracks_the_workspace_version`,
/// runs as a bump prerequisite — before the sed — so it cannot fail the release
/// it protects. This one checks the rewrite is still live, ahead of time.
#[test]
fn the_core_pin_rewrite_still_matches_cargo_toml() {
    let commands = logical_commands(&bump_recipe());
    let core = commands
        .iter()
        .filter(|c| c.contains("sed -i"))
        .find(|c| c.contains("dbd-core"))
        .expect("bump must move the dbd-core pin with the workspace version");

    // `s|<search>|<replace>|` — the search half is what has to still match.
    let search = core.split('|').nth(1).expect("sed uses the s|search|replace| form");
    let cargo = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("Cargo.toml");
    assert!(
        cargo.contains(search),
        "the dbd-core sed no longer matches Cargo.toml, so bump would skip it silently:\n  {search}"
    );
}

/// The help must name the branch the gate actually checks.
///
/// `_check-clean` compares HEAD against `origin/<current branch>`, but the help
/// said `origin/main`. A maintainer reads that, merges develop → main and bumps
/// from there — inverting the order the recipe itself prescribes one line
/// later ("Now merge $(BRANCH) → main") — or gets refused with a message naming
/// a ref the help never mentioned.
#[test]
fn the_help_names_the_branch_the_gate_checks() {
    // The gate is branch-parametric — it compares against whichever branch you
    // are on. Assert that from the source, not from an expanded `make -n`:
    // comparing the help against today's branch would let `origin/main` pass
    // whenever the test happens to run on main, which is exactly the bug.
    let makefile = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Makefile")).expect("Makefile");
    assert!(
        makefile.contains("origin/$(BRANCH)"),
        "_check-clean no longer compares against origin/$(BRANCH); this test's premise changed"
    );

    // So the help must promise a placeholder, never a specific branch.
    let named = origin_refs(&make(&["help"]));
    assert!(
        !named.is_empty(),
        "the help names no origin/<ref> at all — the gate's requirement is undocumented"
    );
    for n in &named {
        assert!(
            n.starts_with('<'),
            "help promises origin/{n}, but the gate checks whichever branch you are on"
        );
    }
}

/// One entry per shell statement make will run, with backslash continuations
/// folded back together. `make -n` prints a continued statement across several
/// output lines, so a per-line scan would treat one statement as many — and
/// would miss exactly the property these tests are about: what shares a shell
/// with what.
fn logical_commands(recipe: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in recipe.lines() {
        let continued = line.ends_with('\\');
        current.push_str(line.trim_end_matches('\\'));
        if continued {
            continue;
        }
        out.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The pre-tag build must be able to fail the release.
///
/// `cargo build … | grep …` reports the *pipe's* status, and a trailing
/// `|| true` masks even that — so a build that did not compile would sail
/// through into `git commit`, `git tag` and two `git push`es, publishing a tag
/// for a tree that never built.
#[test]
fn the_prerelease_build_is_not_masked() {
    let recipe = bump_recipe();
    let commands = logical_commands(&recipe);
    let build: Vec<&String> = commands.iter().filter(|c| c.contains("cargo build")).collect();

    assert!(!build.is_empty(), "bump must build before it tags:\n{recipe}");
    for line in &build {
        assert!(
            !line.contains('|'),
            "piping hides cargo's exit status behind the pipe's: {line}"
        );
        assert!(
            !line.contains("|| true"),
            "`|| true` discards the build's failure outright: {line}"
        );
    }
}

/// `make -n bump` must stay a dry run.
///
/// make executes any recipe line containing `$(MAKE)` even under `-n`, so a
/// recursive call sharing a shell statement with `cargo clean` turns the dry
/// run into a real one and wipes `target/`. That is invisible in the printed
/// recipe — the only way to see it is to look for the side effect.
#[test]
fn a_dry_run_bump_does_not_touch_the_build_dir() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sentinel = root.join("target/.dry-run-sentinel");
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(&sentinel, "must survive a dry run").unwrap();

    let _ = bump_recipe();

    let survived = sentinel.exists();
    let _ = fs::remove_file(&sentinel);
    assert!(
        survived,
        "`make -n bump` deleted target/ — a dry run executed a real command"
    );
}

/// Both pushes must share one statement.
///
/// `git push origin <branch>` and `git push origin <tag>` as separate recipe
/// lines can half-succeed: the version commit goes public, the tag does not,
/// make aborts with no guidance. Because `VERSION` is then read back from the
/// already-bumped Cargo.toml, a re-run cuts the *next* version and the
/// half-published one is never tagged.
#[test]
fn both_pushes_share_one_statement_with_a_resume_path() {
    let pushes: Vec<String> = logical_commands(&bump_recipe())
        .into_iter()
        .filter(|c| c.contains("git push"))
        .collect();

    assert_eq!(
        pushes.len(),
        1,
        "the branch and tag pushes must share a statement so a half-push can report how to \
         resume; found {} separate ones:\n  {}",
        pushes.len(),
        pushes.join("\n  ")
    );
    assert!(
        pushes[0].contains("git push origin v") || pushes[0].contains("git push origin \"v"),
        "the resume path must name the tag push explicitly: {}",
        pushes[0]
    );
}

/// The post-push statement must survive its own failures.
///
/// It runs after the tag is public, so every exit from it has to leave the tree
/// reclaimed and the operator told what to do. Three properties, each of which
/// was absent and unguarded: the captured install status is re-raised, the
/// clean's own status is inspected rather than assumed, and an interrupt is
/// trapped so Ctrl-C during a multi-minute install still explains itself.
#[test]
fn the_post_push_statement_reports_every_way_it_can_fail() {
    let statement = logical_commands(&bump_recipe())
        .into_iter()
        .find(|c| c.contains("cargo clean"))
        .expect("bump must reclaim the build directory");

    assert!(
        statement.contains("exit $ok"),
        "a failed install must fail the recipe after the clean, not just print: {statement}"
    );
    assert!(
        statement.contains("if cargo clean"),
        "the clean's exit status must be inspected before claiming target/ is gone: {statement}"
    );
    assert!(
        statement.contains("trap ") && statement.contains("INT"),
        "Ctrl-C after the push must still explain that the release is complete: {statement}"
    );
}

/// A failed install must not strand the working tree.
///
/// `install` runs after both pushes, so the release is already public by then;
/// if it fails, make aborts the recipe and the `cargo clean` that reclaims the
/// build directory never runs. Re-running `make bump` to recover cuts a *new*
/// version instead. Keeping the clean in the same shell statement as the
/// install makes it unconditional.
#[test]
fn a_failed_install_still_reclaims_the_build_dir() {
    let recipe = bump_recipe();
    let commands = logical_commands(&recipe);

    let cleans: Vec<&String> = commands.iter().filter(|c| c.contains("cargo clean")).collect();
    assert_eq!(cleans.len(), 1, "expected exactly one clean step:\n{recipe}");

    assert!(
        cleans[0].contains("install"),
        "`cargo clean` must share a shell statement with the install so an \
         install failure cannot skip it; found it standing alone: {}",
        cleans[0]
    );
}
