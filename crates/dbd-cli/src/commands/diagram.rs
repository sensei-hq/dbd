use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use super::safe_write;
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub fn cmd_diagram(
    config: &Path,
    env: &str,
    project_dir: &Path,
    file: &Path,
    json: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let model = dbd_core::schema_model::build(&design, Some(&resolved));
    if json {
        let s = serde_json::to_string_pretty(&model)
            .context("Failed to serialize schema model")?;
        safe_write(project_dir, file, &s)?;
        output::info(verbosity, &format!("Wrote schema model to {}", file.display()));
    } else {
        let html = dbd_core::diagram::render_html(&model)
            .context("Failed to render HTML diagram")?;
        safe_write(project_dir, file, &html)?;
        output::info(verbosity, &format!("Wrote schema diagram to {}", file.display()));
    }
    Ok(())
}
