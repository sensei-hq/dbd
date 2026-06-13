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
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
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
    if !print_url && let Err(e) = open::that(&url) {
        output::info(verbosity, &format!("(couldn't open a browser: {e}); open the URL above)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_site_prefers_explicit_then_default() {
        assert_eq!(resolve_site(Some("http://localhost:5173")), "http://localhost:5173");
        assert_eq!(resolve_site(None), DEFAULT_SITE);
    }
}
