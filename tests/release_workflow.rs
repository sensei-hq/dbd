//! Guards on the tag-triggered crates.io publish workflow.
//!
//! Publishing is the one release step that cannot be undone — a version can be
//! yanked but never replaced — so the properties that matter are all about what
//! has to be true *before* the upload, and they are properties of the workflow
//! file rather than of any Rust code. Asserted against the YAML text: these are
//! guards against the file being edited into something that publishes too
//! eagerly, not a re-implementation of Actions.

use std::fs;
use std::path::{Path, PathBuf};

fn workflow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml")
}

fn workflow() -> String {
    let p = workflow_path();
    fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — the release is not complete without a publish step; \
             `make bump` deliberately does not publish, so nothing else does",
            p.display()
        )
    })
}

/// The upload must be gated on the suite, in the runner, on the tagged tree.
///
/// `make bump` runs the suite locally before tagging, but it runs it on the
/// maintainer's machine and *before* the version seds land — so it never sees
/// the tree that is actually released. A publish that trusts that is trusting a
/// different commit on a different machine. Since the upload is irreversible,
/// the workflow re-runs the suite on the tag itself and publishes only after.
#[test]
fn the_publish_is_gated_on_the_test_suite() {
    let wf = workflow();
    let test_at = wf
        .find("cargo test")
        .expect("release.yml must run the suite before uploading anything");
    let publish_at = wf.find("cargo publish").expect("release.yml must publish");
    assert!(
        test_at < publish_at,
        "release.yml publishes before it tests — an irreversible upload must never precede its gate"
    );
}

/// The tag and the manifest version must agree before publishing.
///
/// The two are set by different steps (`git tag` vs a `sed` over Cargo.toml), so
/// nothing structural keeps them equal. Publishing on a mismatch uploads a
/// version nobody tagged, under a tag that names something else — and neither
/// can be withdrawn.
#[test]
fn the_tag_is_checked_against_the_manifest_version() {
    let wf = workflow();
    assert!(
        wf.contains("Cargo.toml") && (wf.contains("ref_name") || wf.contains("inputs.tag")),
        "release.yml must compare the tag it was triggered by against the version in Cargo.toml"
    );
    let guard_at = wf.find("Cargo.toml").expect("checked above");
    let publish_at = wf.find("cargo publish").expect("release.yml must publish");
    assert!(
        guard_at < publish_at,
        "the version guard must run before the upload, not after"
    );
}

/// Triggered by the tag, and resumable by hand.
///
/// The tag is this project's release artifact, so it is the correct trigger. But
/// a publish can fail for reasons that have nothing to do with the code — a
/// rejected token, a registry outage — and a public tag must never be deleted
/// and re-pushed to retry. So a manual trigger taking an existing tag has to
/// exist alongside the automatic one.
#[test]
fn the_workflow_triggers_on_tags_and_can_be_replayed_by_hand() {
    let wf = workflow();
    assert!(
        wf.contains("tags:"),
        "release.yml must trigger on tag pushes — the tag is the release"
    );
    assert!(
        wf.contains("workflow_dispatch:"),
        "release.yml needs a manual trigger, so a failed publish can be retried without \
         deleting and re-pushing a public tag"
    );
}

/// Third-party actions are pinned to a commit, as in ci.yml.
///
/// A mutable tag like `@v4` lets the action's author change what runs in a job
/// that holds a crates.io token with publish rights. ci.yml already pins every
/// `uses:` to a SHA; this file handles a secret and must not be looser.
#[test]
fn third_party_actions_are_pinned_to_a_sha() {
    for line in workflow().lines() {
        let Some((_, spec)) = line.split_once("uses:") else {
            continue;
        };
        let spec = spec.trim();
        // A local action (`./.github/...`) has no version to pin.
        if spec.starts_with('.') {
            continue;
        }
        let (_, git_ref) = spec
            .split_once('@')
            .unwrap_or_else(|| panic!("unpinned action, no @ref at all: {spec}"));
        let sha = git_ref.split_whitespace().next().unwrap_or("");
        assert!(
            sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "action must be pinned to a full commit SHA, got `{sha}`: {spec}"
        );
    }
}
