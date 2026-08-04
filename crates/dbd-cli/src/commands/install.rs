use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use console::style;

use crate::output::{self, Verbosity};

/// Claude Code assets embedded in the binary. Kept byte-for-byte in sync with
/// the canonical copies under `docs/` (enforced by `embedded_assets_match_docs`
/// below). Embedding — rather than reading `docs/` or downloading — lets a
/// `cargo install dbd-cli` binary install them with no repo, network, or extra
/// runtime deps, matching dbd's "one binary, zero runtime deps" promise.
const SKILL_MD: &str = include_str!("../assets/skills/dbd/SKILL.md");
const AGENT_MD: &str = include_str!("../assets/agents/dbd-pattern-verifier.md");

/// One installable asset: its label, path under a `.claude` root, and text.
struct Asset {
    kind: &'static str,
    /// Path relative to the `.claude` base, e.g. `skills/dbd/SKILL.md`.
    rel: &'static str,
    contents: &'static str,
}

const ASSETS: &[Asset] = &[
    Asset { kind: "skill", rel: "skills/dbd/SKILL.md", contents: SKILL_MD },
    Asset { kind: "agent", rel: "agents/dbd-pattern-verifier.md", contents: AGENT_MD },
];

/// What installing one asset would do to the target path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// No file at the destination yet.
    Create,
    /// A file exists but differs from the bundled asset.
    Update,
    /// The destination already matches the bundled asset.
    Unchanged,
}

/// Resolve the `.claude` base directory the assets install under.
///
/// - `--project` → `<project_dir>/.claude` (honours the global `--source`).
/// - global (default) → `$CLAUDE_CONFIG_DIR` if set (Claude Code's own config
///   override), else `$HOME/.claude`.
fn claude_base(project: bool, project_dir: &Path) -> Result<PathBuf> {
    if project {
        return Ok(project_dir.join(".claude"));
    }
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .context("cannot locate home directory: $HOME is unset — use --project to install into ./.claude")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// Decide what installing `contents` to `dest` would do, without writing.
///
/// `dest` is always `base.join(asset.rel)` where `asset.rel` is a compile-time
/// constant and `base` is a directory the invoking user chose (their own
/// `$CLAUDE_CONFIG_DIR`/`$HOME`/`--project`). No untrusted input reaches this
/// path, so the traversal warning below is a false positive for a local CLI.
fn classify(dest: &Path, contents: &str) -> Result<Action> {
    match fs::read(dest) { // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        Ok(existing) if existing == contents.as_bytes() => Ok(Action::Unchanged),
        Ok(_) => Ok(Action::Update),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Action::Create),
        Err(e) => Err(e).with_context(|| format!("reading {}", dest.display())),
    }
}

/// Write `contents` to `dest`, creating parent directories as needed.
fn write_asset(dest: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    fs::write(dest, contents).with_context(|| format!("writing {}", dest.display()))
}

/// Install dbd's Claude Code skill + agent under the resolved `.claude` base.
///
/// The assets are dbd-owned and namespaced (`skills/dbd/`, `agents/dbd-pattern-verifier.md`),
/// so re-running after a dbd upgrade refreshes them in place; each action
/// (created/updated/unchanged) is reported so an overwrite is never silent.
pub fn cmd_install(
    project: bool,
    dry_run: bool,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
    let base = claude_base(project, project_dir)?;
    output::info(
        verbosity,
        &format!(
            "{} dbd Claude Code assets → {}",
            if dry_run { "Would install" } else { "Installing" },
            base.display()
        ),
    );

    let (mut created, mut updated, mut unchanged) = (0u32, 0u32, 0u32);
    for asset in ASSETS {
        let dest = base.join(asset.rel);
        let action = classify(&dest, asset.contents)?;
        let (sym, label) = match action {
            Action::Create => {
                created += 1;
                (style("+").green(), "created")
            }
            Action::Update => {
                updated += 1;
                (style("~").yellow(), "updated")
            }
            Action::Unchanged => {
                unchanged += 1;
                (style("=").dim(), "unchanged")
            }
        };
        output::info(
            verbosity,
            &format!("  {sym} {} ({}, {label})", dest.display(), asset.kind),
        );
        if !dry_run && action != Action::Unchanged {
            write_asset(&dest, asset.contents)?;
        }
    }

    output::info(
        verbosity,
        &format!(
            "{}: {created} created, {updated} updated, {unchanged} unchanged.",
            if dry_run { "Would write" } else { "Wrote" }
        ),
    );
    if !dry_run && (created + updated) > 0 {
        output::detail(verbosity, "Restart Claude Code to load the new skill/agent.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The embedded assets MUST stay byte-for-byte identical to the canonical
    /// copies under `docs/` (the source the website and docs serve). If someone
    /// edits `docs/skills/dbd/SKILL.md` without refreshing the embedded copy,
    /// installed binaries would ship stale content — this test fails first.
    #[test]
    fn embedded_assets_match_docs() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skill = fs::read_to_string(repo.join("docs/skills/dbd/SKILL.md"))
            .expect("read canonical docs/skills/dbd/SKILL.md");
        let agent = fs::read_to_string(repo.join("docs/agents/dbd-pattern-verifier.md"))
            .expect("read canonical docs/agents/dbd-pattern-verifier.md");
        assert_eq!(
            SKILL_MD, skill,
            "embedded SKILL.md drifted from docs/skills/dbd/SKILL.md — re-copy it into \
             crates/dbd-cli/src/assets/skills/dbd/SKILL.md"
        );
        assert_eq!(
            AGENT_MD, agent,
            "embedded agent drifted from docs/agents/dbd-pattern-verifier.md — re-copy it into \
             crates/dbd-cli/src/assets/agents/dbd-pattern-verifier.md"
        );
    }

    /// Every bundled asset advertises a schema-correct destination: skills live at
    /// `skills/<name>/SKILL.md`, agents at `agents/<name>.md`.
    #[test]
    fn asset_paths_are_well_formed() {
        for a in ASSETS {
            assert!(!a.contents.is_empty(), "{} asset is empty", a.kind);
            match a.kind {
                "skill" => assert!(
                    a.rel.starts_with("skills/") && a.rel.ends_with("/SKILL.md"),
                    "skill path {} is malformed",
                    a.rel
                ),
                "agent" => assert!(
                    a.rel.starts_with("agents/") && a.rel.ends_with(".md"),
                    "agent path {} is malformed",
                    a.rel
                ),
                other => panic!("unexpected asset kind {other}"),
            }
        }
    }

    /// `--project` resolves the base to `<project_dir>/.claude` (no env involved).
    #[test]
    fn project_base_is_under_project_dir() {
        let base = claude_base(true, Path::new("/tmp/proj")).unwrap();
        assert_eq!(base, Path::new("/tmp/proj/.claude"));
    }

    /// `classify` reports Create for a missing path, Unchanged for a byte match,
    /// and Update when the existing file differs.
    #[test]
    fn classify_detects_create_unchanged_update() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("SKILL.md");

        assert_eq!(classify(&dest, "hello").unwrap(), Action::Create);

        let mut f = fs::File::create(&dest).unwrap();
        f.write_all(b"hello").unwrap();
        assert_eq!(classify(&dest, "hello").unwrap(), Action::Unchanged);
        assert_eq!(classify(&dest, "goodbye").unwrap(), Action::Update);
    }
}
