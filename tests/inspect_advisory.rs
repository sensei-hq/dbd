//! End-to-end tests for `dbd inspect`'s advisory enum section.
//!
//! The unit tests around `render_enum_hints` cover the rendering; these cover
//! what only the assembled command can show — that the advisory block is
//! *placed* below nothing and *counted* into nothing. No fixture in the repo
//! produces an enum hint, so without these the whole advisory path in
//! `cmd_inspect` is dead code under test: it could be folded into the blocking
//! error count and every test would still pass.

use std::fs;
use std::path::Path;

use assert_cmd::Command;

/// A minimal project whose one table has a string-set CHECK — the shape that
/// makes `inspect` emit an enum suggestion.
fn project_with_one_enum_candidate(dir: &Path) {
    fs::write(
        dir.join("design.yaml"),
        "project:\n  name: advisory\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let table = dir.join("ddl/table/app");
    fs::create_dir_all(&table).unwrap();
    fs::write(
        table.join("orders.ddl"),
        "set search_path to app;\n\n\
         create table if not exists orders (\n  \
           id    integer primary key\n, \
           state text not null constraint orders_state_chk check (state in ('pending', 'shipped'))\n\
         );\n",
    )
    .unwrap();
}

fn inspect(dir: &Path) -> std::process::Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .args(["inspect", "-c"])
        .arg(dir.join("design.yaml"))
        .arg("--source")
        .arg(dir)
        .output()
        .unwrap()
}

/// An advisory is not an error: it never reaches the summary's error count and
/// never moves the exit code. This is the invariant most at risk — folding
/// `enum_hints` into `blocking` sits three lines away in the source and would
/// make every project with a string-set CHECK exit 1 from a clean design.
#[test]
fn enum_advisory_is_not_counted_as_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    project_with_one_enum_candidate(tmp.path());

    let out = inspect(tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "an advisory must not fail the run: status {:?}\n{stdout}",
        out.status.code()
    );
    assert!(
        stdout.contains("no issues"),
        "the summary must report a clean design: {stdout}"
    );
    assert!(
        stdout.contains("(1 enum suggestion(s) — advisory)"),
        "the advisory is counted on its own line: {stdout}"
    );
}

/// The summary is the last thing on screen. Printed above the advisory block it
/// scrolls away behind the suggestions, which is backwards — the counts are
/// what a reader is looking for.
#[test]
fn summary_is_printed_below_the_advisory_block() {
    let tmp = tempfile::tempdir().unwrap();
    project_with_one_enum_candidate(tmp.path());

    let out = inspect(tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let suggestions = stdout.find("Suggestions:").unwrap_or_else(|| {
        panic!("no advisory section — the fixture stopped producing a hint:\n{stdout}");
    });
    let proposal = stdout
        .find("ddl/enum/app/state.ddl")
        .unwrap_or_else(|| panic!("no proposal line: {stdout}"));
    let summary = stdout
        .find("no issues")
        .unwrap_or_else(|| panic!("no summary line: {stdout}"));

    assert!(
        suggestions < proposal && proposal < summary,
        "order must be Suggestions: -> proposals -> summary, got {suggestions}/{proposal}/{summary}:\n{stdout}"
    );
}
