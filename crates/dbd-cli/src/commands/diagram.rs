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
    _json: bool, // v1: always JSON; flag reserved for v2 when HTML becomes the default
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;
    let model = dbd_core::schema_model::build(&design, Some(&resolved));
    let json = serde_json::to_string_pretty(&model)
        .context("Failed to serialize schema model")?;
    safe_write(project_dir, file, &json)?;
    output::info(verbosity, &format!("Wrote schema model to {}", file.display()));
    Ok(())
}
