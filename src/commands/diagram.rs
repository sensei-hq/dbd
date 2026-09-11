use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use super::safe_write;
use crate::output::{self, Verbosity};

/// Default hosted dbd site (override with --site or $DBD_DIAGRAM_URL).
const DEFAULT_SITE: &str = "https://dbd.sensei-hq.com";

/// Resolve the site base URL: an explicit value (flag or $DBD_DIAGRAM_URL, both
/// surfaced by clap as `site`) wins; otherwise the built-in default.
fn resolve_site(site: Option<&str>) -> &str {
    site.unwrap_or(DEFAULT_SITE)
}

/// Whether a URL may be handed to the platform's URL opener.
///
/// The base comes from `--site` or `$DBD_DIAGRAM_URL`, so the string that
/// reaches [`open::that`] is not one dbd composed end to end. That matters
/// because the opener is not uniform: on Windows it goes through
/// `cmd /c start`, where a metacharacter in the argument is a command rather
/// than text, and every platform's opener will happily act on a non-web scheme
/// (`file://`, `javascript:`, a UNC path) by launching whatever is registered
/// for it.
///
/// So this allows exactly the two schemes a diagram can be served over and
/// nothing else. The URL is printed to stdout unconditionally, so a refusal
/// costs the operator a click, not the output.
fn is_browsable_url(url: &str) -> bool {
    let rest = match url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        Some(rest) => rest,
        None => return false,
    };
    // A shell metacharacter cannot appear in a host or in a correctly-encoded
    // path/fragment, so its presence means the string is not just a URL.
    !rest.is_empty() && !rest.contains(['&', '|', ';', '"', '\'', '`', '\n', '\r', '\0'])
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_diagram(
    config: &Path,
    env: &str,
    project_dir: &Path,
    json: bool,
    file: &Path,
    print_url: bool,
    site: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let model = dbd_core::schema_model::build(&design, Some(&resolved));

    if json {
        let s = serde_json::to_string_pretty(&model).context("Failed to serialize schema model")?;
        safe_write(project_dir, file, &s)?;
        output::info(verbosity, &format!("Wrote schema model to {}", file.display()));
        return Ok(());
    }

    let base = resolve_site(site);
    let url = dbd_core::diagram::fragment_url(base, &model).context("Failed to encode diagram URL")?;
    if url.len() > 1_500_000 {
        output::info(
            verbosity,
            "Note: this schema produces a very large URL; if the browser truncates it, run `dbd diagram --json` and upload the file at the site instead.",
        );
    }
    // The URL is the command's data output — always to stdout (pipeable).
    println!("{url}");
    if !print_url {
        if !is_browsable_url(&url) {
            output::info(
                verbosity,
                "(not opening a browser: --site/$DBD_DIAGRAM_URL is not a plain http(s) URL); open the URL above)",
            );
        } else if let Err(e) = open::that(&url) {
            output::info(
                verbosity,
                &format!("(couldn't open a browser: {e}); open the URL above)"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--site` / `$DBD_DIAGRAM_URL` is operator-supplied and reaches the
    /// platform's URL opener, which on Windows runs `cmd /c start` — where a
    /// metacharacter in the argument is a command, not text. Only the two schemes
    /// a diagram can actually be served over may be handed over; the URL is
    /// printed to stdout regardless, so refusing to auto-open costs nothing.
    #[test]
    fn only_http_urls_are_handed_to_the_browser_opener() {
        assert!(is_browsable_url("https://dbd.sensei-hq.com/#x"));
        assert!(is_browsable_url("http://localhost:5173/#x"));

        for hostile in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "https:evil & calc.exe",
            "\\\\attacker\\share\\payload.exe",
            "",
        ] {
            assert!(!is_browsable_url(hostile), "{hostile:?} must not be opened");
        }
    }

    #[test]
    fn resolve_site_prefers_explicit_then_default() {
        assert_eq!(resolve_site(Some("http://localhost:5173")), "http://localhost:5173");
        assert_eq!(resolve_site(None), DEFAULT_SITE);
    }

    use crate::commands::testutil;

    /// `--json` builds the schema model and writes it under the project.
    #[test]
    fn diagram_json_writes_model_file() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let out = proj.path().join("model.json");
        cmd_diagram(
            &cfg,
            "dev",
            proj.path(),
            true,
            &out,
            false,
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .unwrap();
        assert!(out.exists());
    }

    /// URL mode with `print_url = true` encodes + prints the URL and skips the
    /// browser-open (so it's safe and DB-free in tests).
    #[test]
    fn diagram_url_prints_without_opening_browser() {
        cmd_diagram(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            false,
            std::path::Path::new("unused.json"),
            /*print_url*/ true,
            Some("http://localhost:5173"),
            None,
            None,
            Verbosity::Normal,
        )
        .unwrap();
    }
}
