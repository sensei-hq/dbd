//! Every Rust example in the embedder-facing docs must compile.
//!
//! # Why this exists
//!
//! `Design::apply` takes five arguments. Every documented example showed seven,
//! on **six** surfaces — README, both `SKILL.md` copies, `architecture.md`
//! twice, `llms-full.txt`, the live site and the design mockups — because the
//! method's own doc comment said "use `|_| {}` / `|_, _| {}` / `|_| {}`" and
//! everyone copied the misreading. Not one of those examples compiled. Nothing
//! noticed, because nothing compiled them.
//!
//! A doctest fixed the source (`Design::apply` now carries one). This fixes the
//! copies.
//!
//! # How
//!
//! Two halves, and both are needed:
//!
//! - [`doc_examples_generated.rs`](../doc_examples_generated.rs) holds every
//!   example wrapped in a function. Cargo **compiles** it as an ordinary test
//!   target, so an example that does not typecheck is a build failure.
//! - The test below re-extracts from the docs and compares. That is what keeps
//!   the generated file from going stale when someone edits a doc — compiling
//!   an out-of-date copy would prove nothing about what users read.
//!
//! Regenerate after editing a doc example:
//!
//! ```text
//! DBD_REGEN_DOC_EXAMPLES=1 cargo test --test doc_examples
//! ```
//!
//! # Scope, and why it stops where it does
//!
//! The **embedder-facing** surfaces only — what somebody copies into their own
//! crate. `docs/design/architecture.md` holds 34 blocks that are design prose
//! (pipeline sketches, `When: design.apply()` notes), and compiling those would
//! mean rewriting a design document into valid Rust for no reader's benefit.
//!
//! An individual block opts out with ```` ```rust,ignore ````.

use std::path::{Path, PathBuf};

/// The surfaces a user copies code from.
const SURFACES: &[&str] = &[
    "README.md",
    "docs/skills/dbd/SKILL.md",
    "src/assets/skills/dbd/SKILL.md",
    "docs/llms/llms-full.txt",
    // The architecture document's copyable examples. Its other 22 blocks are
    // illustrative — signatures, sketches, aspirational test files — and are
    // fenced ```rust,ignore so they keep their highlighting while staying out
    // of this gate. Compiling a sketch would mean rewriting a design document
    // into working Rust for no reader's benefit; compiling a call somebody
    // pastes is the whole point.
    "docs/design/architecture.md",
];

/// Names a doc example may use without declaring. The docs write `parse_sql(sql)`
/// rather than inventing a string literal, which reads better and needs a stub
/// here — supplied once rather than by editing every example.
const PREAMBLE: &str = "\
    #[allow(unused_variables)] let sql: &str = \"\";\n    \
    #[allow(unused_variables)] let database_url: &str = \"\";\n    \
    #[allow(unused_variables)] let db_url: &str = \"\";\n    \
    #[allow(unused_variables)] let db_version: u32 = 0;\n    \
    #[allow(unused_variables)] let entities: Vec<dbd_core::Entity> = Vec::new();\n    \
    #[allow(unused_variables)] let project_dir = std::path::Path::new(\".\");\n    \
    #[allow(unused_variables)] let path = std::path::Path::new(\"design.yaml\");\n";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn generated_path() -> PathBuf {
    root().join("tests/doc_examples_generated.rs")
}

/// Every ```` ```rust ```` block in a file, in order. `rust,ignore` is skipped.
fn blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let fence = line.trim();
        if !fence.starts_with("```rust") {
            continue;
        }
        // `rust,ignore` opts a block out — for one that documents a shape
        // rather than a call.
        let skip = fence.contains("ignore");
        let mut body = String::new();
        for line in lines.by_ref() {
            if line.trim() == "```" {
                break;
            }
            body.push_str(line);
            body.push('\n');
        }
        if !skip {
            out.push(body);
        }
    }
    out
}

/// Wrap a fragment so it typechecks: an `async fn` returning dbd's `Result`, so
/// `?` and `.await` work exactly as they do in the doc.
fn wrap(name: &str, body: &str) -> String {
    format!(
        "#[allow(unused, clippy::all)]\n\
         async fn {name}() -> dbd_core::Result<()> {{\n{PREAMBLE}{}\n    Ok(())\n}}\n\n",
        body.trim_end()
    )
}

/// A digest of every extracted block, in order.
///
/// The sync test compares THIS rather than the generated file's bytes, because
/// `cargo fmt` reformats the generated file and a byte comparison would fail on
/// formatting rather than on drift. The digest is over what the docs say, which
/// is the thing that must not change silently.
fn docs_digest() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for surface in SURFACES {
        let text = std::fs::read_to_string(root().join(surface)).unwrap_or_else(|e| panic!("{surface}: {e}"));
        for block in blocks(&text) {
            block.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn generate() -> String {
    let mut out = format!(
        "// @generated by tests/doc_examples.rs — do not edit.\n\
         //\n\
         // Every Rust example in the embedder-facing docs, wrapped so it can be\n\
         // compiled. Cargo builds this as a test target, so an example that does\n\
         // not typecheck fails the build. See tests/doc_examples.rs for why.\n\
         //\n\
         // Regenerate: DBD_REGEN_DOC_EXAMPLES=1 cargo test --test doc_examples\n\
         //\n\
         // docs-digest: {:#018x}\n\
         #![allow(dead_code, unused_imports, non_snake_case)]\n\n",
        docs_digest()
    );
    for surface in SURFACES {
        let text = match std::fs::read_to_string(root().join(surface)) {
            Ok(t) => t,
            Err(e) => panic!("{surface}: {e}"),
        };
        let slug: String = Path::new(surface)
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        for (i, body) in blocks(&text).into_iter().enumerate() {
            out.push_str(&format!("// {surface} — block {i}\n"));
            out.push_str(&wrap(&format!("doc_{slug}_{i}"), &body));
        }
    }
    out
}

/// The generated file must match what the docs say *now*.
///
/// Compiling a stale copy would prove something about a doc nobody reads any
/// more. This is the half that keeps the other half honest.
#[test]
fn the_compiled_examples_match_the_docs() {
    let generated = generate();
    let path = generated_path();

    if std::env::var("DBD_REGEN_DOC_EXAMPLES").is_ok() {
        std::fs::write(&path, &generated).expect("write generated examples");
        println!("regenerated {}", path.display());
        return;
    }

    let _ = generated;
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    let want = format!("docs-digest: {:#018x}", docs_digest());
    assert!(
        committed.contains(&want),
        "\n\nThe compiled doc examples are out of date with the docs.\n\
         A doc example changed, so the file cargo actually compiles no longer\n\
         matches what a reader is told to write.\n\n\
         Regenerate and re-run:\n\
         \x20   DBD_REGEN_DOC_EXAMPLES=1 cargo test --test doc_examples\n"
    );
}

/// The surfaces list must name files that exist. A typo here would silently
/// stop gating a surface — the gate would pass by checking nothing.
#[test]
fn every_named_surface_exists() {
    for surface in SURFACES {
        let path = root().join(surface);
        assert!(path.is_file(), "{surface} is gated but does not exist");
    }
}

/// And it must find examples. If an extraction bug returned nothing, every
/// test here would pass while compiling an empty file.
#[test]
fn the_extraction_actually_finds_examples() {
    let mut total = 0usize;
    for surface in SURFACES {
        let text = std::fs::read_to_string(root().join(surface)).unwrap();
        total += blocks(&text).len();
    }
    assert!(
        total >= 5,
        "only {total} doc examples extracted — the extractor is probably broken, \
         and a gate that checks nothing passes for free"
    );
}
