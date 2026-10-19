use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;
use dbd_core::design::Progress;

use super::{format_deploy_summary, get_adapter, safe_copy, safe_read, safe_write};
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub fn cmd_graph(
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    module: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let reading = super::reading(config, env, project_dir, module, scope, deps)?;
    let resolved = reading.scope();
    let graph = reading.graph(name)?;
    // A file dbd could not parse is missing from the nodes, as it is from the
    // DBML and the diagram, and is named the same way — on stderr, which
    // `left_out_unparsed` writes to, so stdout stays the JSON document.
    output::left_out_unparsed(&reading.unparsed()?);

    let json = serde_json::json!({
        "nodes": graph.nodes.iter().map(|n| {
            let mut node = serde_json::json!({
                "name": n.name, "type": n.entity_type, "schema": n.schema,
            });
            // A workspace's node names the module declaring it (#7).
            if let Some(module) = &n.module {
                node["module"] = module.as_str().into();
            }
            node
        }).collect::<Vec<_>>(),
        "edges": graph.edges.iter().map(|e| serde_json::json!({
            "from": e.from, "to": e.to,
        })).collect::<Vec<_>>(),
        "layers": graph.layers,
        // Named in the document rather than printed beside it: stdout here is a
        // JSON contract (`dbd graph | jq`), and a graph that silently omits a
        // schema is indistinguishable from a complete one. Null for the
        // all-scope, which filters nothing.
        "scope": (!resolved.is_all).then(|| resolved.name.clone()),
    });

    output::always(&serde_json::to_string_pretty(&json)?);
    // On stderr: printed after the document on stdout, the count made
    // `dbd graph -v | jq` fail to parse.
    if verbosity.is_verbose() {
        eprintln!(
            "{} nodes, {} edges, {} layers",
            graph.nodes.len(),
            graph.edges.len(),
            graph.layers.len()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_dbml(
    config: &Path,
    env: &str,
    project_dir: &Path,
    file: &Path,
    module: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    // Filtered to the scope's working set, so the generated DBML documents only
    // the entities that deploy under this scope — each module's, in a
    // workspace. The all-scope keeps everything.
    let reading = super::reading(config, env, project_dir, module, scope, deps)?;
    let (kept, total) = reading.counts()?;
    output::scope_filtered(reading.scope(), kept, total);
    output::left_out_unparsed(&reading.unparsed()?);

    let docs = dbd_core::dbml::generate_reading(&reading)?;

    // Single document → use the user-supplied path verbatim. Multiple
    // documents → write each to `<parent_of(file)>/<doc.file_name>`,
    // preserving the user's directory choice while honoring each doc's
    // configured filename.
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let written = match docs.len() {
        0 => 0,
        1 => {
            safe_write(project_dir, file, &docs[0].content)?;
            output::info(verbosity, &format!("Generated DBML in {}", file.display()));
            1
        }
        _ => {
            for doc in &docs {
                let path = dir.join(&doc.file_name);
                safe_write(project_dir, &path, &doc.content)?;
                output::info(verbosity, &format!("Generated DBML in {}", path.display()));
            }
            docs.len()
        }
    };
    output::detail(verbosity, &format!("Wrote {written} DBML document(s)"));
    Ok(())
}

/// `dbd split --from-scopes` (#7): the plan, and with `--write` the project
/// made a workspace of it.
#[allow(clippy::too_many_arguments)]
pub fn cmd_split(
    config: &Path,
    env: &str,
    project_dir: &Path,
    from_scopes: bool,
    write: bool,
    renames: &[String],
    verbosity: Verbosity,
) -> Result<()> {
    if !from_scopes {
        anyhow::bail!(
            "dbd split needs --from-scopes — the one way it divides a project yet: a module for each region of its \
             scopes"
        );
    }
    if let Some(journal) = dbd_core::split::interrupted(project_dir) {
        anyhow::bail!(
            "a dbd split was interrupted here — its journal {} lists the files it moved: run `dbd split --undo` to \
             put them back, then split again",
            journal.display()
        );
    }
    if let Ok(read) = dbd_core::config::read(config)
        && !read.modules.is_empty()
    {
        anyhow::bail!("{} declares modules: — it is a workspace already", config.display());
    }
    let renames: Vec<(String, String)> = renames
        .iter()
        .map(|r| {
            r.split_once('=')
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .ok_or_else(|| anyhow::anyhow!("--rename {r}: write it MODULE=NAME, as --rename shared=common"))
        })
        .collect::<Result<_>>()?;
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let plan = dbd_core::split::plan_from_scopes(&design, &renames)?;

    let width = plan.modules.iter().map(|m| m.name.len()).max().unwrap_or(0);
    let regions: Vec<String> = plan.modules.iter().map(|m| region_phrase(&m.region, &plan)).collect();
    let region_width = regions.iter().map(String::len).max().unwrap_or(0);
    output::always(&format!(
        "{} splits into {} modules, by its scopes:",
        plan.project,
        plan.modules.len()
    ));
    for (m, region) in plan.modules.iter().zip(&regions) {
        let count = match m.entities.len() {
            1 => "1 entity".to_string(),
            n => format!("{n} entities"),
        };
        let depends = if m.depends_on.is_empty() {
            String::new()
        } else {
            format!("   depends on {}", m.depends_on.join(", "))
        };
        output::always(&format!(
            "  {:width$}   {region:region_width$}   {count}{depends}",
            m.name
        ));
        if verbosity.is_verbose() {
            for entity in &m.entities {
                output::always(&format!("      {entity}"));
            }
        }
    }
    output::always("Its scopes become:");
    for s in &plan.scopes {
        match &s.modules {
            None => output::always(&format!("  {}: all", s.name)),
            Some(modules) => {
                let excludes = if s.excludes.is_empty() {
                    String::new()
                } else {
                    format!(", excludes [{}]", s.excludes.join(", "))
                };
                let extensions = s
                    .extensions
                    .as_ref()
                    .map(|e| format!(", extensions [{}]", e.join(", ")))
                    .unwrap_or_default();
                output::always(&format!(
                    "  {}: modules [{}]{excludes}{extensions}",
                    s.name,
                    modules.join(", ")
                ));
            }
        }
    }
    if verbosity.is_verbose() {
        for m in &plan.moves {
            output::always(&format!("  {} → {}", m.from.display(), m.to.display()));
        }
    }
    if let Some(cycle) = &plan.cycle {
        output::warn(&format!(
            "the modules' dependencies would cycle — {} — move entities so they run one way; --write is refused \
             until they do",
            cycle.join(" → ")
        ));
    }
    let scripts = plan.script_paths.len();
    let files = plan.moves.len() - scripts;
    let moving = match scripts {
        0 => format!("{files} files move into the modules' folders"),
        1 => format!("{files} files move into the modules' folders, and 1 import script to scripts/"),
        n => format!("{files} files move into the modules' folders, and {n} import scripts to scripts/"),
    };
    if !write {
        output::always(&format!(
            "{moving}. Run again with --write to move them and write modules: and scopes: into design.yaml."
        ));
        return Ok(());
    }
    dbd_core::split::write_split(&plan, config, project_dir)?;
    let moved = match scripts {
        0 => format!("Moved {files} files into the modules' folders"),
        1 => format!("Moved {files} files into the modules' folders, and 1 import script to scripts/"),
        n => format!("Moved {files} files into the modules' folders, and {n} import scripts to scripts/"),
    };
    output::always(&format!(
        "{moved}, and wrote modules: and scopes: into {}. Each module's first deploy takes over what it declares \
         from \"{}\".",
        config.display(),
        plan.project
    ));
    Ok(())
}

/// `dbd split --undo` (#7): what an interrupted `--write` moved, put back.
pub fn cmd_split_undo(config: &Path, project_dir: &Path) -> Result<()> {
    let moved = dbd_core::split::undo(project_dir)?;
    output::always(&format!(
        "Put back {moved} files; {} is as it was before the split.",
        config.display()
    ));
    Ok(())
}

/// Which scopes a module's region is in, as the plan says it.
fn region_phrase(region: &[String], plan: &dbd_core::split::SplitPlan) -> String {
    let scopes = plan.scopes.iter().filter(|s| s.modules.is_some()).count();
    let listed = match region {
        [] => return "in no scope".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    if region.len() == scopes && scopes > 1 {
        format!("in {listed}")
    } else {
        format!("only in {listed}")
    }
}

pub fn cmd_doctor(
    config: &Path,
    source_dir: &Path,
    module: Option<&str>,
    fix: bool,
    verbosity: Verbosity,
) -> Result<()> {
    if !config.exists() {
        anyhow::bail!("Config file not found: {}", config.display());
    }

    let project_dir = config.parent().unwrap_or(Path::new("."));
    let content = safe_read(project_dir, config)?;

    let config_issues = dbd_core::doctor::detect_old_format(&content);
    // A workspace's files are each module's, in its folder under --source, as
    // every other command reads them (#7). A design.yaml this dbd cannot read
    // — one written for an older dbd, what doctor exists to migrate — is
    // audited as a project's.
    let parsed = dbd_core::config::read(config);
    let unreadable = parsed.as_ref().err().map(|e| e.to_string());
    let parsed_config = parsed.ok();
    let folders: Vec<std::path::PathBuf> = match &parsed_config {
        Some(_) => super::file_folders(config, source_dir, module)?
            .into_iter()
            .map(|(_, dir)| dir)
            .collect(),
        None if module.is_some() => {
            anyhow::bail!(
                "--module: {} does not read as a design.yaml: {}",
                config.display(),
                unreadable.unwrap_or_default()
            );
        }
        None => vec![project_dir.to_path_buf()],
    };
    // A design.yaml declaring `modules:` is not one an older dbd wrote — it
    // had none — so what reads as old in it is fixed by hand: migrating it
    // would rebuild it without its modules.
    let workspace_manifest =
        serde_yaml::from_str::<serde_yaml::Value>(&content).is_ok_and(|v| v.get("modules").is_some());
    // What reads as neither today's design.yaml nor an older one is said, not
    // audited clean.
    let unreadable = unreadable.filter(|_| config_issues.is_empty());
    let (mut stale_files, mut plural_dirs, mut mismatches, mut declared_files) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for dir in &folders {
        stale_files.extend(dbd_core::doctor::detect_stale_files(dir));
        plural_dirs.extend(dbd_core::doctor::detect_plural_ddl_dirs(dir));
        // Misfiled view/matview DDL — detection only; the fix is a manual move.
        mismatches.extend(dbd_core::doctor::detect_ddl_type_mismatches(dir));
        // Extension/schema files the scanner does not read — detection only;
        // the fix is a design.yaml entry, named for each.
        declared_files.extend(dbd_core::doctor::detect_declared_kind_files(
            dir,
            parsed_config.as_ref(),
        ));
    }

    let config_fixable = if workspace_manifest { 0 } else { config_issues.len() };
    let auto_fixable = config_fixable + stale_files.len() + plural_dirs.len();
    let by_hand = mismatches.len()
        + declared_files.len()
        + (config_issues.len() - config_fixable)
        + usize::from(unreadable.is_some());
    let total_issues = auto_fixable + by_hand;
    if let Some(error) = &unreadable {
        output::always(&format!("design.yaml does not read: {error}"));
    }

    // doctor counts issues, not entities: the shared entity summary printed
    // "0 entities — no issues" here on a project full of tables.
    if total_issues == 0 {
        output::info(verbosity, "No issues found — project is up to date.");
        return Ok(());
    }

    report_doctor_issues(&config_issues, &stale_files, &plural_dirs, &mismatches);
    report_declared_kind_files(&declared_files);

    if !fix {
        if auto_fixable > 0 {
            output::always("\nRun with --fix to resolve automatically.");
        }
        if !mismatches.is_empty() {
            output::always("Misfiled DDL files must be moved manually (see above).");
        }
        if !declared_files.is_empty() {
            output::always("Extension and schema files must be replaced by hand (see above).");
        }
        output::always(&format!("\n{} found.", issues(total_issues)));
        return Ok(());
    }

    let mut fixed = 0;
    if !config_issues.is_empty() && workspace_manifest {
        output::always(
            "\ndesign.yaml declares modules:, so its old entries (above) are fixed by hand — migrating it \
             would rebuild it without its modules.",
        );
    } else if !config_issues.is_empty() {
        fix_config_migration(config, project_dir, &content, verbosity)?;
        fixed += config_issues.len();
    }
    if !stale_files.is_empty() {
        fixed += fix_stale_files(&stale_files, verbosity);
    }
    if !plural_dirs.is_empty() {
        fixed += fix_plural_dirs(&plural_dirs, verbosity);
    }
    // Misfiled DDL is not auto-fixed: report it as remaining so the summary is
    // honest, and reset stays broken until the file is moved.
    if !mismatches.is_empty() {
        output::always(&format!(
            "\n{} misfiled DDL file{} left unchanged — move manually:",
            mismatches.len(),
            if mismatches.len() != 1 { "s" } else { "" }
        ));
        for m in &mismatches {
            output::always(&format!("  - {} → {}", m.path.display(), m.suggested_path.display()));
        }
    }
    if !declared_files.is_empty() {
        output::always(&format!(
            "\n{} left unchanged — replace by hand (see above).",
            files(declared_files.len(), "extension/schema file")
        ));
    }
    output::always(&format!("\n{} fixed, {by_hand} left to fix by hand.", issues(fixed)));

    Ok(())
}

/// `1 <what>` / `N <what>s`.
fn files(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// Print each `ddl/extension/` and `ddl/schema/` file with the design.yaml
/// entry that replaces it.
fn report_declared_kind_files(found: &[dbd_core::doctor::DeclaredKindFile]) {
    use dbd_core::doctor::DeclaredKind;
    if found.is_empty() {
        return;
    }
    output::always(&format!(
        "\nFound {} dbd does not read (design.yaml declares extensions and schemas):",
        files(found.len(), "DDL file")
    ));
    for f in found {
        let fix = match (f.kind, f.declared) {
            (DeclaredKind::Extension, true) => format!(
                "`{}` is already in the target's extensions: list — delete the file",
                f.name
            ),
            (DeclaredKind::Extension, false) => {
                let entry = match &f.schema {
                    None => format!("`{}`", f.name),
                    Some(schema) => format!("`{{ name: {}, schema: {schema} }}`", f.name),
                };
                format!("add {entry} to the target's extensions: list in design.yaml, then delete the file")
            }
            (DeclaredKind::Schema, true) => format!("`{}` is already listed under schemas: — delete the file", f.name),
            (DeclaredKind::Schema, false) => {
                format!("list `{}` under schemas: in design.yaml, then delete the file", f.name)
            }
        };
        output::always(&format!("  - {} → {fix}", f.path.display()));
    }
}

/// `1 issue` / `N issues`.
fn issues(n: usize) -> String {
    format!("{n} issue{}", if n == 1 { "" } else { "s" })
}

/// Print the config / stale-file / plural-folder issues doctor detected.
fn report_doctor_issues(
    config_issues: &[String],
    stale_files: &[dbd_core::doctor::StaleFile],
    plural_dirs: &[dbd_core::doctor::PluralDdlDir],
    mismatches: &[dbd_core::doctor::DdlTypeMismatch],
) {
    if !config_issues.is_empty() {
        output::always(&format!(
            "Found {} config issue{}:",
            config_issues.len(),
            if config_issues.len() != 1 { "s" } else { "" }
        ));
        for issue in config_issues {
            output::always(&format!("  - {issue}"));
        }
    }

    if !stale_files.is_empty() {
        output::always(&format!(
            "\nFound {} stale file{} (now managed internally by dbd):",
            stale_files.len(),
            if stale_files.len() != 1 { "s" } else { "" }
        ));
        for f in stale_files {
            output::always(&format!("  - {} — {}", f.path.display(), f.reason));
        }
    }

    if !plural_dirs.is_empty() {
        output::always(&format!(
            "\nFound {} plural DDL folder{} (singular is canonical):",
            plural_dirs.len(),
            if plural_dirs.len() != 1 { "s" } else { "" }
        ));
        for d in plural_dirs {
            output::always(&format!("  - {} → {}", d.plural.display(), d.singular.display()));
        }
    }

    if !mismatches.is_empty() {
        output::always(&format!(
            "\nFound {} misfiled DDL file{} (folder disagrees with the CREATE statement):",
            mismatches.len(),
            if mismatches.len() != 1 { "s" } else { "" }
        ));
        for m in mismatches {
            output::always(&format!(
                "  - {} declares CREATE {} but lives in ddl/{}/",
                m.path.display(),
                m.declared.replace('_', " ").to_uppercase(),
                m.folder
            ));
            output::always(&format!("      → move to {}", m.suggested_path.display()));
        }
    }
}

/// Migrate an old-format config in place, validating the result and backing up
/// the original to a `.yaml.bak` sibling first.
fn fix_config_migration(config: &Path, project_dir: &Path, content: &str, verbosity: Verbosity) -> Result<()> {
    let migrated = dbd_core::doctor::migrate_config(content).context("Config migration failed")?;

    let _: dbd_core::config::DesignConfig =
        serde_yaml::from_str(&migrated).context("Migrated config failed to parse — please report this as a bug")?;

    let backup = config.with_extension("yaml.bak");
    safe_copy(project_dir, config, &backup)?;
    output::info(verbosity, &format!("Backup saved to {}", backup.display()));

    safe_write(project_dir, config, &migrated)?;
    output::info(verbosity, &format!("Migrated {}", config.display()));
    Ok(())
}

/// Remove stale internally-managed files, returning how many were removed.
fn fix_stale_files(stale_files: &[dbd_core::doctor::StaleFile], verbosity: Verbosity) -> usize {
    let mut fixed = 0;
    let results = dbd_core::doctor::remove_stale_files(stale_files);
    for (path, err) in &results {
        match err {
            None => {
                output::info(verbosity, &format!("Removed {}", path.display()));
                fixed += 1;
            }
            Some(e) => {
                output::always(&format!("Failed to remove {}: {e}", path.display()));
            }
        }
    }
    fixed
}

/// Migrate plural DDL folders to their singular canonical form, returning how
/// many folders were migrated in full.
///
/// A folder is one issue — that is how `doctor` finds and reports it — so it
/// is one fixed, however many files moving it took. Counting the moves, doctor
/// found "2 issues" and fixed "4". A folder any of whose files failed to move
/// is not fixed.
fn fix_plural_dirs(plural_dirs: &[dbd_core::doctor::PluralDdlDir], verbosity: Verbosity) -> usize {
    use dbd_core::doctor::DdlMoveOutcome;
    let mut fixed = 0;
    for d in plural_dirs {
        let mut all_moved = true;
        for outcome in dbd_core::doctor::migrate_plural_ddl_dir(d) {
            match outcome {
                DdlMoveOutcome::RenamedDir { from, to } => {
                    output::info(verbosity, &format!("Renamed {} → {}", from.display(), to.display()));
                }
                DdlMoveOutcome::MovedFile { from, to } => {
                    output::info(verbosity, &format!("Moved {} → {}", from.display(), to.display()));
                }
                DdlMoveOutcome::BackedUp {
                    winner,
                    loser,
                    final_path,
                    backup,
                } => {
                    output::always(&format!(
                        "Collision at {}: kept newer (from {}), backed up older (from {}) → {}",
                        final_path.display(),
                        winner.display(),
                        loser.display(),
                        backup.display()
                    ));
                }
                DdlMoveOutcome::Error { path, error } => {
                    output::always(&format!("Failed to migrate {}: {error}", path.display()));
                    all_moved = false;
                }
            }
        }
        if all_moved {
            fixed += 1;
        }
    }
    fixed
}

pub fn cmd_init(project_dir: &Path, name: &str, target: &str, verbosity: Verbosity) -> Result<()> {
    let files = dbd_core::init::create_project(project_dir, name, target).context("Failed to initialize project")?;

    output::info(verbosity, &format!("Initialized dbd project '{name}' ({target})"));
    for file in &files {
        if !file.content.is_empty() {
            output::detail(verbosity, &format!("  created {}", file.path.display()));
        }
    }
    output::info(verbosity, "\nNext steps:");
    output::info(verbosity, "  1. Set DATABASE_URL or edit design.yaml target.url");
    output::info(verbosity, "  2. Add DDL files to ddl/table/<schema>/<name>.ddl");
    output::info(verbosity, "  3. Run 'dbd inspect' to validate");
    output::info(verbosity, "  4. Run 'dbd apply' to create schema in database");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_deploy(
    source: &str,
    config_name: &Path,
    env: &str,
    database_url: Option<&str>,
    dry_run: bool,
    no_cache: bool,
    clear_cache: bool,
    allow_scope_change: bool,
    ownership: &super::OwnershipChange,
    module: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    output::info(verbosity, &format!("Deploying from source: {source}"));

    if clear_cache {
        dbd_core::deploy::clear_cache().context("Failed to clear cache")?;
        output::info(verbosity, "Cleared download cache");
    }

    let project_dir = dbd_core::deploy::resolve_source(source, no_cache)
        .await
        .context("Failed to resolve source")?;

    // `config_name` is `-c` relative to the source, so it names the same file
    // in a downloaded GitHub tree as in a local one.
    let config_path = project_dir.join(config_name);
    if !config_path.exists() {
        anyhow::bail!("No {} found in {}", config_name.display(), project_dir.display());
    }

    let targets: Vec<super::Target> = super::targets(
        &config_path,
        env,
        &project_dir,
        scope,
        deps,
        module,
        None,
        super::Order::Build,
    )
    .context("Failed to load design from source")?
    .into_iter()
    .map(|target| target.map_design(|design| ownership.applied_to(design)))
    .collect();
    let (targets, emptied) = super::emptied_together(targets, None, scope, deps)?;
    let run = DeployRun {
        config_path: &config_path,
        database_url,
        dry_run,
        allow_scope_change,
        scope,
        deps,
        verbosity,
    };
    if targets.len() > 1 && !dry_run {
        preflight_deploy(&targets, &run).await?;
        return deploy_workspace(targets, &emptied, &run).await;
    }
    for (i, target) in targets.into_iter().enumerate() {
        let (mut design, dir) = target.enter(i == 0);
        deploy_design(&mut design, &dir, &run).await?;
        super::say_if_retired(&design);
    }
    Ok(())
}

/// A workspace's deploy: every module's schema, then every module's data
/// (#7). Its import empties what the run reloads together, before the first
/// module loads — so no module may migrate after that, reading a table emptied
/// for a reload that has not happened yet.
async fn deploy_workspace(targets: Vec<super::Target>, emptied: &super::Emptied, run: &DeployRun<'_>) -> Result<()> {
    let verbosity = run.verbosity;
    let mut built = Vec::new();
    for (i, target) in targets.into_iter().enumerate() {
        let label = target.label();
        let (design, _) = target.enter(i == 0);
        let resolved = design
            .resolve_scope(run.scope, run.deps)
            .context("Failed to resolve scope")?;
        let (kept, total) = design.scope_counts(&resolved)?;
        output::scope_filtered(&resolved, kept, total);
        for warning in design.search_path_warnings(None, Some(&resolved))? {
            output::warn(&warning);
        }
        let adapter = get_adapter(run.config_path, run.database_url, design.owner()).await?;
        let marks = design.database_marks(&*adapter).await?;
        Design::check_scope_guard(&marks, &resolved.name, run.allow_scope_change)?;
        super::announce_adoptions(&design, &*adapter, false).await?;
        for skip in design.hook_skips(Some(&resolved))? {
            output::warn(&skip);
        }
        output::info(verbosity, "Applying schema...");
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .deploy_schema(
                &*adapter,
                false,
                Some(&resolved),
                &mut |desc: &str| spinner.start(desc),
                &mut |desc: &str, err: Option<&str>| spinner.done(desc, err),
            )
            .await;
        spinner.finish();
        let (apply, grants) = result.context("Deploy failed")?;
        super::note_promotion(&marks, design.env());
        output::info(
            verbosity,
            &format!(
                "{}{}",
                super::format_apply_summary(&apply),
                super::format_grants(&grants)
            ),
        );
        super::say_if_retired(&design);
        built.push((label, design, adapter, resolved));
    }

    output::always("");
    output::info(verbosity, "Loading data...");
    for (i, (label, design, adapter, resolved)) in built.into_iter().enumerate() {
        if let Some(label) = label {
            output::always(&if i == 0 { label } else { format!("\n{label}") });
        }
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .deploy_data(
                &*adapter,
                false,
                Some(&resolved),
                &mut |desc: &str| spinner.start(desc),
                &mut |desc: &str, err: Option<&str>| spinner.done(desc, err),
            )
            .await;
        spinner.finish();
        let (import, policies) = result
            .context("Deploy failed")
            .map_err(|e| emptied.explain(i, i > 0 || design.emptied_for_import(), e))?;
        for warning in &import.warnings {
            output::warn(warning);
        }
        if !policies.applied.is_empty() {
            output::info(
                verbosity,
                &format!("Applied {} policy file(s).", policies.applied.len()),
            );
        }
        let summary = dbd_core::design::DeployComplete {
            policies,
            ..Default::default()
        };
        for warning in summary.policy_warnings() {
            output::warn(&warning);
        }
        output::info(
            verbosity,
            &format!(
                "{} {}",
                super::format_import_summary(&import),
                super::format_policy_line(&summary.policies)
            ),
        );
    }
    Ok(())
}

/// Before the first module of a workspace deploys, each module's deploy
/// checked as its dry run checks it — its scope pin, its guards, what it would
/// claim, its migrations, hooks and imports — so a refusal any module's run
/// would meet comes before any module has written (#7).
async fn preflight_deploy(targets: &[super::Target], run: &DeployRun<'_>) -> Result<()> {
    for target in targets {
        let design = &target.design;
        let check = async {
            let adapter = get_adapter(run.config_path, run.database_url, design.owner()).await?;
            let resolved = design.resolve_scope(run.scope, run.deps)?;
            let marks = design.database_marks(&*adapter).await?;
            Design::check_scope_guard(&marks, &resolved.name, run.allow_scope_change)?;
            design.deploy(&*adapter, true, Some(&resolved), |_| {}).await?;
            anyhow::Ok(())
        };
        target.labelled(check.await)?;
    }
    Ok(())
}

/// What `dbd deploy` was asked, for each design it deploys.
struct DeployRun<'a> {
    config_path: &'a Path,
    database_url: Option<&'a str>,
    dry_run: bool,
    allow_scope_change: bool,
    scope: Option<&'a str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
}

/// [`cmd_deploy`] for one design: the project's, or one module's.
async fn deploy_design(design: &mut Design, project_dir: &Path, run: &DeployRun<'_>) -> Result<()> {
    let &DeployRun {
        config_path,
        database_url,
        dry_run,
        allow_scope_change,
        scope,
        deps,
        verbosity,
    } = run;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    // What the scope builds, not what the design declares: a scoped deploy
    // never touches the rest, so the whole-design tally described a run that
    // does not happen. Named for the run as for its preview.
    let (kept, total) = design.scope_counts(&resolved)?;
    output::scope_filtered(&resolved, kept, total);
    // Each file its apply executes on a path it does not state, as `apply`
    // says it — a forgotten search_path is never silent.
    for warning in design.search_path_warnings(None, Some(&resolved))? {
        output::warn(&warning);
    }

    if dry_run {
        // The real deploy's own gate (it runs `Design::apply`): scope gaps and
        // closure, the extension allowlist, and the refusal of a file that did
        // not parse. The preview used to tally "1 errors" and exit 0 over a
        // design the deploy refuses.
        let to_apply = design.entities_to_apply(None, Some(&resolved))?.len();
        // With a database to ask, refuse where the deploy refuses: the scope
        // pin, then its apply's environment and version guards — the import's
        // environment guard is the same one.
        let adapter = super::dry_run_adapter(config_path, database_url, design.owner()).await?;
        if let Some(adapter) = &adapter {
            let marks = design.database_marks(&**adapter).await?;
            Design::check_scope_guard(&marks, &resolved.name, allow_scope_change)?;
            design.check_run_guards(&**adapter, &marks).await?;
            // The real run claims the design's entities; its preview refuses
            // what that claim would, and names what it would adopt (#7).
            design.check_ownership(&**adapter).await?;
            super::announce_adoptions(design, &**adapter, true).await?;
        }
        // The migrations its apply phase executes first, built before anything
        // is printed: it refuses where the run does — an unresolved `-- TODO`
        // in a pending data script — and a refused preview should not list a
        // plan first, as this one listed its entity count.
        let preview = match &adapter {
            Some(adapter) => Some(
                design
                    .pending_migration_preview(&**adapter, None, Some(&resolved))
                    .await?,
            ),
            None => None,
        };
        // The hooks the run would skip, decided as the run decides them.
        for skip in design.hook_skips(Some(&resolved))? {
            output::warn(&skip);
        }

        let report = design.report(None, Some(&resolved));
        if !resolved.is_all {
            output::scope_gaps(&resolved, &report.gaps);
        }
        output::info(verbosity, &format!("{to_apply} entities would be applied."));
        // The migrations its apply phase executes first, as `apply --dry-run` lists them.
        match &preview {
            Some(preview) => super::print_migration_preview(verbosity, preview),
            None => super::note_unlisted_migrations(verbosity, project_dir),
        }

        // Preview the import the same way the real run reports it: always state
        // the file count — including zero — and why anything was left out.
        let (import_plan, skips) = design.scoped_import_plan(None, Some(&resolved))?;
        output::info(
            verbosity,
            &format!("{} data file(s) would be imported.", import_plan.len()),
        );
        for warning in design.import_warnings(None).into_iter().chain(skips) {
            output::warn(&warning);
        }
        // The import's `import.after` hooks, judged as `import --dry-run` and
        // the real deploy judge them.
        let (_, after_skips) = design.import_after_preview(Some(&resolved))?;
        for warning in after_skips {
            output::warn(&warning);
        }

        // The policy phase's own scope filter, counted the way the real
        // deploy's summary counts it.
        let policy_ws = super::schema::policy_working_set(design, &resolved)?;
        let policies =
            dbd_core::design::plan_policies(project_dir, policy_ws.as_ref().map(|(n, ws)| (n.as_str(), ws)))?;
        output::info(
            verbosity,
            &format!(
                "{} policy file(s) would be applied{}.",
                policies.applied.len(),
                match policies.skipped.len() {
                    0 => String::new(),
                    n => format!(", {n} skipped (out of scope)"),
                }
            ),
        );
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    let adapter = get_adapter(config_path, database_url, design.owner()).await?;
    let marks = design.database_marks(&*adapter).await?;
    Design::check_scope_guard(&marks, &resolved.name, allow_scope_change)?;
    super::announce_adoptions(design, &*adapter, false).await?;

    // Said before the run, not in its summary: a deploy that fails never
    // prints one, and a skipped hook is often why it failed.
    for skip in design.hook_skips(Some(&resolved))? {
        output::warn(&skip);
    }

    // Delegate to the library's `Design::deploy` — apply + import + policies —
    // so `dbd deploy` and an embedder calling the crate run the same pipeline.
    output::info(verbosity, "Applying schema...");
    let mut summary: Option<dbd_core::design::DeployComplete> = None;
    {
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .deploy_with_progress(
                &*adapter,
                false,
                Some(&resolved),
                Progress {
                    on_start: |desc: &str| spinner.start(desc),
                    on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                    on_complete: |s| summary = Some(s),
                },
            )
            .await;
        spinner.finish();
        result.context("Deploy failed")?;
    }
    super::note_promotion(&marks, design.env());

    let summary = summary.unwrap_or_default();
    // Non-fatal diagnostics — skipped imports and failed policy files — are
    // always surfaced, regardless of verbosity. A deploy that quietly loaded
    // nothing is the failure mode this reporting exists to prevent. Each in the
    // order its phase ran — schema, data, policies — so nothing reads as having
    // happened before what it followed.
    // The apply phase's warnings are the hook skips, already said before it.
    for warning in &summary.import.warnings {
        output::warn(warning);
    }
    if !summary.policies.applied.is_empty() {
        output::info(
            verbosity,
            &format!("Applied {} policy file(s).", summary.policies.applied.len()),
        );
    }
    for warning in summary.policy_warnings() {
        output::warn(&warning);
    }
    output::info(verbosity, &format_deploy_summary(&summary));
    Ok(())
}

/// Reconcile the live database to the design in place (pre-release workflow).
///
/// Gated on `project.released`: once a project is released, schema changes must
/// go through `dbd snapshot` + `dbd apply` instead.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_reconcile(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    dry_run: bool,
    allow_destructive: bool,
    prune: dbd_core::Prune,
    allow_scope_change: bool,
    ownership: &super::OwnershipChange,
    module: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let targets: Vec<super::Target> = super::targets(
        config,
        env,
        project_dir,
        scope,
        deps,
        module,
        None,
        super::Order::Reconcile,
    )?
    .into_iter()
    .map(|target| target.map_design(|design| ownership.applied_to(design)))
    .collect();
    // A released module changes by snapshot and `dbd apply`: the one asked for
    // — the project, or the module `--module` names — is refused before
    // anything connects; others the run would take in are left out.
    for target in &targets {
        refuse_a_released_reconcile(&target.design)?;
    }
    if targets.is_empty() {
        anyhow::bail!("Every module this run selects is released — `reconcile` is disabled for them.");
    }
    if targets.len() > 1 && !dry_run {
        // Before the first module writes, each module's reconcile checked as
        // its dry run checks it — so a refusal any module's would meet comes
        // before any module has written (#7).
        for target in &targets {
            let design = &target.design;
            let check = async {
                refuse_a_released_reconcile(design)?;
                let adapter = get_adapter(config, database_url, design.owner()).await?;
                let resolved = design.resolve_scope(scope, deps)?;
                let marks = design.database_marks(&*adapter).await?;
                Design::check_scope_guard(&marks, &resolved.name, allow_scope_change)?;
                design
                    .reconcile(
                        &*adapter,
                        true,
                        allow_destructive,
                        prune,
                        Some(&resolved),
                        Progress::none(),
                    )
                    .await?
                    .refuse_destructive(allow_destructive)?;
                anyhow::Ok(())
            };
            target.labelled(check.await)?;
        }
    }
    for (i, target) in targets.into_iter().enumerate() {
        let (design, _) = target.enter(i == 0);
        reconcile_design(
            &design,
            config,
            database_url,
            dry_run,
            allow_destructive,
            prune,
            allow_scope_change,
            scope,
            deps,
            verbosity,
        )
        .await?;
        super::say_if_retired(&design);
    }
    Ok(())
}

/// A released project — or module — changes through `dbd snapshot` and
/// migrations; `reconcile` is the pre-release workflow.
fn refuse_a_released_reconcile(design: &Design) -> Result<()> {
    if design.config().project.released {
        match design.module() {
            Some(module) => anyhow::bail!(
                "Module {module} is released — `reconcile` is disabled for it. Capture its changes with \
                 `dbd snapshot --module {module}`, then migrate with `dbd apply`."
            ),
            None => anyhow::bail!(
                "Project is released — `reconcile` is disabled. \
                 Capture changes with `dbd snapshot`, then migrate with `dbd apply`."
            ),
        }
    }
    Ok(())
}

/// [`cmd_reconcile`] for one design: the project's, or one module's.
#[allow(clippy::too_many_arguments)]
async fn reconcile_design(
    design: &Design,
    config: &Path,
    database_url: Option<&str>,
    dry_run: bool,
    allow_destructive: bool,
    prune: dbd_core::Prune,
    allow_scope_change: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    refuse_a_released_reconcile(design)?;

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let (kept, total) = design.scope_counts(&resolved)?;
    output::scope_filtered(&resolved, kept, total);
    // A dry run writes nothing, and the SQLite adapter creates a missing file
    // on connect — the preview left an empty database behind. So it opens only
    // a database that is there, as every other dry run does. Only a SQLite
    // file reads as not there, and what the real run would reconcile against
    // is then an empty database: an in-memory one is that, and leaves no file.
    // With no database configured at all, the preview needs one as the run
    // does, and says how to name it.
    let configured = super::configured_database_url(config, database_url)?;
    let adapter = match configured {
        Some(url) if dry_run => {
            let project = design.owner();
            match dbd_core::connect_existing(&url, project)
                .await
                .context("Failed to connect to database")?
            {
                Some(adapter) => adapter,
                None => dbd_core::connect("sqlite::memory:", project)
                    .await
                    .context("Failed to open an empty database to preview against")?,
            }
        }
        _ => get_adapter(config, database_url, design.owner()).await?,
    };
    reconcile_with_adapter(
        &*adapter,
        design,
        &resolved,
        dry_run,
        allow_destructive,
        prune,
        allow_scope_change,
        verbosity,
    )
    .await
}

/// The body of `dbd reconcile`, with the adapter supplied rather than connected.
///
/// Split out so what a run decides against a given database — the scope guard
/// above all — is testable against a mock without a live one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reconcile_with_adapter(
    adapter: &dyn dbd_core::DatabaseAdapter,
    design: &Design,
    resolved: &dbd_core::ResolvedScope,
    dry_run: bool,
    allow_destructive: bool,
    prune: dbd_core::Prune,
    allow_scope_change: bool,
    verbosity: Verbosity,
) -> Result<()> {
    // Guarded before the dry run, not after it. The preview connects anyway, so
    // it can see the pin, and a plan for a run the guard will refuse is a
    // preview of something that cannot happen. (`reset --dry-run` asks the
    // same of a configured database; with none configured it previews the
    // plan alone, having no pin to read.)
    let marks = design.database_marks(adapter).await?;
    Design::check_scope_guard(&marks, &resolved.name, allow_scope_change)?;
    super::announce_adoptions(design, adapter, dry_run).await?;

    // The hooks the run skips, said before it — a run that fails prints no
    // summary, and a skipped hook is often why it failed.
    for skip in design.hook_skips(Some(resolved))? {
        output::warn(&skip);
    }

    if dry_run {
        let plan = design
            .reconcile(
                adapter,
                true,
                allow_destructive,
                prune,
                Some(resolved),
                Progress::none(),
            )
            .await
            .context("Reconcile planning failed")?;
        print_reconcile_plan(&plan, prune != dbd_core::Prune::Off, verbosity);
        // The run goes on to the apply hooks and the grants; a preview of it
        // that stops at the schema changes leaves them out.
        for hook in design.apply_hooks_to_run(Some(resolved))? {
            output::info(verbosity, &format!("  run {hook}"));
        }
        let grants = design.grants_to_apply(Some(resolved))?;
        if !grants.is_empty() {
            if adapter.supports_schema_grants() {
                for line in grants {
                    output::info(verbosity, &format!("  grant on {line}"));
                }
            } else {
                output::info(verbosity, "  Skipping schema grants (target has no grant model).");
            }
        }
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    output::info(verbosity, "Reconciling schema to design...");
    let mut summary = None;
    let plan = {
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .reconcile(
                adapter,
                false,
                allow_destructive,
                prune,
                Some(resolved),
                Progress {
                    on_start: |desc: &str| spinner.start(desc),
                    on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                    on_complete: |s| summary = Some(s),
                },
            )
            .await;
        spinner.finish();
        result.context("Reconcile failed")?
    };
    super::note_promotion(&marks, design.env());

    for w in &plan.warnings {
        output::always(&format!("⚠ {w}"));
    }
    // Orphaned tables left untouched (only pruned with --prune).
    if prune == dbd_core::Prune::Off && !plan.dropped.is_empty() {
        output::always(&format!(
            "{} orphaned table(s) not in the design were left untouched (re-run with --prune to drop):",
            plan.dropped.len()
        ));
        for s in &plan.dropped {
            output::always(&format!("    {}", s.entity_name));
        }
    }
    if let Some(s) = summary {
        let hooks = match s.before_scripts + s.after_scripts {
            0 => String::new(),
            n => format!(", {n} hook script(s) run"),
        };
        output::info(
            verbosity,
            &format!(
                "Reconciled — {} created, {} altered, {} re-applied, {} re-stamped, {} pruned{hooks}.",
                s.created, s.altered, s.reapplied, s.restamped, s.dropped
            ),
        );
    }

    // As after `dbd apply`: the design's grants are part of the database it
    // describes, and a reconciled one had none.
    super::apply_and_report_grants(design, adapter, resolved, verbosity).await?;
    Ok(())
}

/// Print a reconcile plan (used by `--dry-run`).
fn print_reconcile_plan(plan: &dbd_core::ReconcilePlan, prune: bool, verbosity: Verbosity) {
    for line in reconcile_plan_lines(plan, prune, verbosity) {
        output::always(&line);
    }
}

/// Build the display lines for a reconcile plan. Pure so it can be unit-tested.
///
/// In verbose mode each altered entity's ALTER SQL is emitted (indented) beneath
/// its summary line; normal mode shows only the one-line-per-entity summary,
/// matching the CLI convention that SQL is a `-v` detail.
fn reconcile_plan_lines(plan: &dbd_core::ReconcilePlan, prune: bool, verbosity: Verbosity) -> Vec<String> {
    // `is_empty()` already covers added/altered/dropped/matview_creates/
    // matview_restamps, but warnings live in their own channel — a plan whose
    // ONLY content is a matview drift warning must still print it rather than
    // falsely claiming "in sync".
    if plan.is_empty() && plan.dropped.is_empty() && plan.warnings.is_empty() {
        return vec!["Already in sync — no changes.".to_string()];
    }
    let mut lines = Vec::new();
    for name in &plan.added {
        lines.push(format!("  + create {name}"));
    }
    for name in &plan.matview_creates {
        lines.push(format!("  + create materialized view {name}"));
    }
    for name in &plan.matview_restamps {
        // Not a `~ alter` (no structural change) and not a `⚠` (not drift): a
        // v1 sentinel's hash isn't comparable to a current one, so upgrading it
        // is a metadata-only write — but still a live write worth previewing.
        lines.push(format!(
            "  ↻ restamp materialized view {name} (v1 hash sentinel upgraded to v2, no drift)"
        ));
    }
    for s in &plan.altered {
        lines.push(format!("  ~ alter  {}", s.entity_name));
        // Verbose surfaces the column-level ALTER SQL beneath the summary line.
        if verbosity.is_verbose() {
            for sql_line in s.sql.lines() {
                lines.push(format!("      {sql_line}"));
            }
        }
    }
    for s in &plan.dropped {
        if prune {
            lines.push(format!("  - prune  {}", s.entity_name));
        } else {
            lines.push(format!("  · orphan {} (use --prune to drop)", s.entity_name));
        }
    }
    for w in &plan.warnings {
        lines.push(format!("  ⚠ {w}"));
    }
    if plan.destructive && !plan.altered.is_empty() {
        lines.push("This plan drops columns/constraints — re-run with --allow-destructive to apply.".to_string());
    }
    lines
}

/// Release the current version: set the released flag, locking the project into
/// the snapshot/migration workflow — writing the baseline snapshot first when
/// the project has none.
///
/// A greenfield project reaches release with no snapshots: it was iterated with
/// `reconcile`, and release cuts its baseline. A brownfield one does not —
/// `init --from-db` writes the baseline as it creates the project, and `merge`
/// snapshots each import — but neither sets `released`. Release used to refuse
/// any project with snapshots, so a brownfield project could never be released
/// and `reconcile` stayed enabled on it for good. Its history is already the
/// record, so it is released at the version that history reached; writing a
/// second baseline over it would discard the versions it already has.
pub fn cmd_release(
    config: &Path,
    env: &str,
    project_dir: &Path,
    module: Option<&str>,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    // Each module releases on its own: the one named, or every module not
    // released yet — each at its own version, its baseline in its folder.
    // Every one is checked before any is written: a release refused for one
    // module leaves design.yaml as it was.
    let targets = super::targets(
        config,
        env,
        project_dir,
        Some("all"),
        None,
        module,
        None,
        super::Order::Alone,
    )?;
    let whole_workspace = module.is_none() && targets.iter().any(|t| t.module.is_some());
    let mut releases = Vec::new();
    for target in targets {
        if whole_workspace && target.design.config().project.released {
            output::info(
                verbosity,
                &format!(
                    "module {} is already released.",
                    target.module.as_deref().unwrap_or_default()
                ),
            );
            continue;
        }
        let release = target.labelled(check_release(&target.design, &target.dir))?;
        releases.push((target, release));
    }
    if whole_workspace && releases.is_empty() {
        anyhow::bail!("Every module is already released.");
    }
    for (i, (target, release)) in releases.into_iter().enumerate() {
        let (design, dir) = target.enter(i == 0);
        write_release(&design, config, &dir, release, name, verbosity)?;
    }
    Ok(())
}

/// What a release of one design does, decided before anything is written.
enum Release {
    /// No snapshot yet: a baseline at the design's version.
    Baseline { version: u32 },
    /// Its snapshots are its history: released at the latest.
    AtLatest { version: u32 },
}

/// Decide [`cmd_release`] for one design, refusing what it would refuse —
/// writing nothing.
fn check_release(design: &Design, project_dir: &Path) -> Result<Release> {
    if design.config().project.released {
        anyhow::bail!(match design.module() {
            Some(module) => format!("Module {module} is already released."),
            None => "Project is already released.".to_string(),
        });
    }
    let error_count = design.entities().iter().filter(|e| !e.errors.is_empty()).count();
    if error_count > 0 {
        anyhow::bail!("Design has {error_count} entity error(s); fix them before releasing (run `dbd inspect`).");
    }
    if !dbd_core::snapshot::has_snapshots(project_dir) {
        return Ok(Release::Baseline {
            version: design.config().project.version(),
        });
    }
    let latest = dbd_core::snapshot::latest_snapshot(project_dir)
        .context("Failed to read the latest snapshot")?
        .context("snapshots/ lists a version but none could be read")?;
    let version = latest.version;

    // The version apply migrates to is `project.version`; releasing at a
    // snapshot version it does not name would release one version and deploy
    // another.
    if let Some(declared) = design.config().project.version
        && declared != version
    {
        let field = match design.module() {
            Some(module) => format!("modules.{module}.version"),
            None => "project.version".to_string(),
        };
        anyhow::bail!(
            "design.yaml says {field} {declared}, but the latest snapshot is v{version} — \
             set {field} to {version} (or restore the missing snapshots) before releasing."
        );
    }

    let pending = dbd_core::snapshot::prepare_multi_snapshot(design.entities(), Some(&latest), version + 1, "release");
    let captured = pending.snapshots.len() == 1 && pending.snapshots[0].no_changes;
    if !captured {
        anyhow::bail!(
            "the design has changes since snapshot v{version} that no snapshot records — \
             capture them with `dbd snapshot`, then run `dbd release` again."
        );
    }
    Ok(Release::AtLatest { version })
}

/// Write the release [`check_release`] decided.
fn write_release(
    design: &Design,
    config: &Path,
    project_dir: &Path,
    release: Release,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    match release {
        Release::Baseline { version } => {
            let desc = name.unwrap_or("baseline release");
            dbd_core::snapshot::create_baseline_snapshot_of(
                design.entities(),
                project_dir,
                config,
                design.module(),
                desc,
                version,
            )
            .context("Failed to create baseline snapshot")?;
            dbd_core::config::set_released_of(config, design.module(), true).context("Failed to set released flag")?;
            output::always(&format!(
                "✓ Released v{version} — baseline snapshot written; `reconcile` is now disabled."
            ));
        }
        Release::AtLatest { version } => {
            dbd_core::config::set_released_of(config, design.module(), true).context("Failed to set released flag")?;
            output::always(&format!(
                "✓ Released v{version} — the existing snapshots are its history, so no baseline was written; \
                 `reconcile` is now disabled."
            ));
            if name.is_some() {
                // `--name` describes the baseline snapshot, and there is none to write.
                output::warn("--name not used: this project's baseline snapshot already exists.");
            }
        }
    }
    output::info(
        verbosity,
        "Next changes: edit the design → `dbd snapshot` → `dbd apply`.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil;
    use dbd_core::reconcile::ReconcileStatement;

    /// A plan with one altered table carrying column-level ALTER SQL.
    fn altered_plan() -> dbd_core::ReconcilePlan {
        dbd_core::ReconcilePlan {
            altered: vec![ReconcileStatement {
                entity_name: "public.users".to_string(),
                sql: "ALTER TABLE public.users ADD COLUMN email text;".to_string(),
            }],
            ..Default::default()
        }
    }

    /// Verbose `--dry-run` surfaces the column-level ALTER SQL under each altered
    /// entity — the detail a plain summary omits.
    #[test]
    fn reconcile_plan_verbose_emits_alter_sql() {
        let out = reconcile_plan_lines(&altered_plan(), false, false, Verbosity::Verbose).join("\n");
        assert!(
            out.contains("~ alter") && out.contains("public.users"),
            "summary line missing:\n{out}"
        );
        assert!(
            out.contains("ADD COLUMN email text"),
            "verbose output must include the ALTER SQL; got:\n{out}"
        );
    }

    /// Normal `--dry-run` stays a terse summary: entity names, no SQL.
    #[test]
    fn reconcile_plan_normal_hides_alter_sql() {
        let out = reconcile_plan_lines(&altered_plan(), false, false, Verbosity::Normal).join("\n");
        assert!(
            out.contains("~ alter") && out.contains("public.users"),
            "summary line missing:\n{out}"
        );
        assert!(
            !out.contains("ADD COLUMN"),
            "normal output must NOT include ALTER SQL; got:\n{out}"
        );
    }

    /// `graph` loads the fixture design and emits the node/edge/layer JSON.
    #[test]
    fn graph_emits_json_from_fixture() {
        cmd_graph(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            None,
            None,
            None,
            Verbosity::Verbose,
        )
        .unwrap();
    }

    /// `dbml` writes generated document(s) under the project — run against a
    /// copy so the repo fixture is never touched.
    #[test]
    fn dbml_writes_into_temp_project() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let out = proj.path().join("schema.dbml");
        cmd_dbml(&cfg, "dev", proj.path(), &out, None, None, None, Verbosity::Normal).unwrap();
    }

    /// Under `--scope`, a Ref to a table the scope leaves out still needs that
    /// table in the document: a Ref to an undefined table is a DBML error, and
    /// the guide promises a stub. The fixture's `incomplete` scope keeps only
    /// `config.lookup_values`, which references `config.lookups` (in the design,
    /// out of scope — so its stub carries the real `uuid`) and
    /// `config.categories` (declared nowhere — so its type is unknown).
    #[test]
    fn dbml_under_a_scope_stubs_every_referenced_table_it_leaves_out() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let out = proj.path().join("scoped.dbml");
        cmd_dbml(
            &cfg,
            "dev",
            proj.path(),
            &out,
            None,
            Some("incomplete"),
            None,
            Verbosity::Normal,
        )
        .unwrap();
        let dbml = std::fs::read_to_string(&out).unwrap();

        let stub = |table: &str| -> String {
            let header = format!("Table {table} {{");
            let start = dbml
                .find(&header)
                .unwrap_or_else(|| panic!("no stub for {table}:\n{dbml}"));
            let body = &dbml[start..];
            body[..body.find('}').unwrap()].to_string()
        };
        let lookups = stub("\"config\".\"lookups\"");
        assert!(
            lookups.contains("\"id\" uuid"),
            "an out-of-scope design table keeps its real type:\n{dbml}"
        );
        assert!(
            !lookups.contains("\"name\""),
            "a stub shows only the referenced columns:\n{dbml}"
        );
        // Nothing defines it, so its key is typed like the column referencing it.
        assert!(stub("\"config\".\"categories\"").contains("\"id\" uuid"), "{dbml}");
    }

    /// A missing config bails with an actionable "not found" error.
    #[test]
    fn doctor_bails_when_config_missing() {
        let missing = std::path::Path::new("/definitely/not/here/design.yaml");
        let err = cmd_doctor(missing, Path::new("."), None, false, Verbosity::Normal).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    /// Doctor is read-only without `--fix` and returns Ok on the fixture.
    #[test]
    fn doctor_runs_read_only_on_fixture() {
        cmd_doctor(
            &testutil::fixture_config(),
            &testutil::fixtures(),
            None,
            false,
            Verbosity::Normal,
        )
        .unwrap();
    }

    /// `init` scaffolds a project into an empty directory.
    #[test]
    fn init_creates_project_files() {
        let tmp = tempfile::tempdir().unwrap();
        cmd_init(tmp.path(), "demo", "postgres", Verbosity::Normal).unwrap();
        assert!(tmp.path().join("design.yaml").exists());
    }

    /// `deploy --dry-run` resolves a local source and reports without applying,
    /// so it never opens a DB connection.
    #[tokio::test]
    async fn deploy_dry_run_from_local_source() {
        let src = testutil::offline_fixture_project();
        cmd_deploy(
            src.path().to_str().unwrap(),
            Path::new("design.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ false,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Deploy treats `policies/` as part of the deploy: a project with a policy
    /// file makes `deploy --dry-run` walk the policy step (scan + report) without
    /// error. Guards against the regression where deploy silently skipped RLS.
    #[tokio::test]
    async fn deploy_dry_run_includes_policies() {
        let proj = testutil::offline_fixture_project();
        std::fs::create_dir_all(proj.path().join("policies")).unwrap();
        std::fs::write(proj.path().join("policies").join("secrets.sql"), "-- rls policy\n").unwrap();
        cmd_deploy(
            proj.path().to_str().unwrap(),
            Path::new("design.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ false,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Releasing writes a baseline snapshot and flips the released flag; a second
    /// release is then refused. Runs against a throwaway copy.
    #[test]
    fn release_writes_baseline_then_refuses_second() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_release(&cfg, "dev", proj.path(), None, Some("v1"), Verbosity::Normal).unwrap();
        let err = cmd_release(&cfg, "dev", proj.path(), None, None, Verbosity::Normal).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("already released") || msg.contains("Snapshots already exist"),
            "expected a released/snapshot guard, got: {msg}"
        );
    }

    /// A fixture copy shaped like `init --from-db` leaves it: a baseline
    /// snapshot at v1 and `project.version: 1`, but no `released` flag.
    fn brownfield_project() -> tempfile::TempDir {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let design = Design::from_config_with_dir(&cfg, "dev", Some(proj.path())).unwrap();
        dbd_core::snapshot::create_baseline_snapshot(design.entities(), proj.path(), &cfg, "init from database", 1)
            .unwrap();
        proj
    }

    /// `init --from-db` and `merge` write snapshots without setting
    /// `released`, and `release` refused any project with snapshots — so a
    /// brownfield project could never be released, and `reconcile` stayed
    /// enabled on it for good. Its baseline already exists: release marks the
    /// project released at that version, and writes no second baseline.
    #[test]
    fn a_brownfield_project_is_released_at_its_snapshot_version() {
        let proj = brownfield_project();
        let cfg = proj.path().join("design.yaml");

        cmd_release(&cfg, "dev", proj.path(), None, None, Verbosity::Normal)
            .expect("a brownfield project can be released");

        let config = dbd_core::config::read(&cfg).unwrap();
        assert!(config.project.released, "the project is marked released");
        assert_eq!(config.project.version, Some(1), "at the version its snapshots reached");
        let versions: Vec<u32> = dbd_core::snapshot::list_snapshots(proj.path())
            .iter()
            .map(|s| s.version)
            .collect();
        assert_eq!(versions, vec![1], "no second baseline is written over the history");
    }

    /// Releasing at the snapshot version is only true if the snapshot is the
    /// design. A change no snapshot captured would ship unversioned, so the
    /// release refuses and says how to capture it.
    #[test]
    fn a_brownfield_release_refuses_changes_no_snapshot_captured() {
        let proj = brownfield_project();
        let cfg = proj.path().join("design.yaml");
        std::fs::write(
            proj.path().join("ddl/table/config/added_later.ddl"),
            "set search_path to config;\ncreate table if not exists added_later (id integer primary key);\n",
        )
        .unwrap();

        let err = cmd_release(&cfg, "dev", proj.path(), None, None, Verbosity::Normal)
            .expect_err("an uncaptured change must not be released as v1");
        assert!(err.to_string().contains("dbd snapshot"), "names the way out: {err}");
        assert!(
            !dbd_core::config::read(&cfg).unwrap().project.released,
            "and the project stays unreleased"
        );
    }

    /// A design with an entity parse error refuses to release — releasing a
    /// broken design would bake the error into the baseline snapshot.
    #[test]
    fn release_refuses_when_an_entity_has_errors() {
        let proj = testutil::copy_fixture_project();
        std::fs::write(
            proj.path().join("ddl/table/config/broken.ddl"),
            "create table config.broken (!!! not valid sql !!!);\n",
        )
        .unwrap();
        let cfg = proj.path().join("design.yaml");

        let err = cmd_release(&cfg, "dev", proj.path(), None, None, Verbosity::Normal).unwrap_err();
        assert!(err.to_string().contains("entity error"), "got: {err}");
    }

    /// `reconcile` is disabled once a project is released — this must be
    /// refused before ever touching a DB adapter.
    #[tokio::test]
    async fn reconcile_refuses_when_project_released() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_release(&cfg, "dev", proj.path(), None, Some("v1"), Verbosity::Normal).unwrap();

        let err = cmd_reconcile(
            &cfg,
            "dev",
            proj.path(),
            None,
            /*dry_run*/ true,
            /*allow_destructive*/ false,
            dbd_core::Prune::Off,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("released"), "got: {err}");
    }

    /// A dry run that connects must refuse the way the real run does. On a
    /// database pinned to `all`, `reconcile --dry-run --scope config_only`
    /// printed a plan and exited 0 — a preview of a run that cannot happen,
    /// since the real run refuses on the scope guard before planning.
    #[tokio::test]
    async fn reconcile_dry_run_refuses_a_database_pinned_to_another_scope() {
        let design =
            Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
        let resolved = design.resolve_scope(Some("config_only"), None).unwrap();
        let mock = dbd_core::adapter::mock::MockAdapter::new().with_scope("all");

        let err = reconcile_with_adapter(
            &mock,
            &design,
            &resolved,
            /*dry_run*/ true,
            /*allow_destructive*/ false,
            dbd_core::Prune::Off,
            /*allow_scope_change*/ false,
            Verbosity::Normal,
        )
        .await
        .expect_err("the dry run must refuse a database pinned to another scope");
        assert!(
            err.to_string().contains("pinned to scope 'all'"),
            "with the real run's reason: {err}"
        );
    }

    /// The guard's own escape hatch reaches the dry run too, so a re-point can
    /// be previewed before it is made.
    #[tokio::test]
    async fn reconcile_dry_run_previews_a_re_point_when_allowed() {
        let design =
            Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
        let resolved = design.resolve_scope(Some("config_only"), None).unwrap();
        let mock = dbd_core::adapter::mock::MockAdapter::new().with_scope("all");

        reconcile_with_adapter(
            &mock,
            &design,
            &resolved,
            /*dry_run*/ true,
            /*allow_destructive*/ false,
            dbd_core::Prune::Off,
            /*allow_scope_change*/ true,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `deploy --clear-cache` clears the local download cache before the rest
    /// of the (local, no-DB) dry-run flow proceeds.
    #[tokio::test]
    async fn deploy_clears_cache_before_dry_run() {
        let src = testutil::offline_fixture_project();
        cmd_deploy(
            src.path().to_str().unwrap(),
            Path::new("design.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ true,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `-c` names the config file inside the source, for deploy as for every
    /// other command. Deploy read `design.yaml` whatever `-c` said.
    #[tokio::test]
    async fn deploy_reads_the_config_named_by_dash_c() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("staging.yaml"), "project:\n  name: other\n").unwrap();
        cmd_deploy(
            src.path().to_str().unwrap(),
            Path::new("staging.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ false,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .expect("deploy -c staging.yaml must read staging.yaml");
    }

    /// A source directory that exists but has no `design.yaml` is a clear,
    /// actionable error — resolved entirely from the local filesystem.
    #[tokio::test]
    async fn deploy_bails_when_source_has_no_design_yaml() {
        let empty = tempfile::tempdir().unwrap();
        let err = cmd_deploy(
            empty.path().to_str().unwrap(),
            Path::new("design.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ false,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("No design.yaml found"), "got: {err}");
    }

    /// A scope with `deps: include` doesn't block on its dependency gap (that's
    /// the whole point of `include`), but `deploy --dry-run` still surfaces the
    /// gap as an FYI — exercising the gap-reporting loop that a `Report`-policy
    /// gap never reaches (it bails earlier, in `check_scope_gaps`).
    ///
    /// Run on a copy without the staging data files: `incomplete_auto` builds
    /// the loaders' targets but not the loaders, so a deploy that loads those
    /// files is refused before it applies anything — not the path under test.
    #[tokio::test]
    async fn deploy_dry_run_reports_gap_for_scope_with_include_deps() {
        // Offline (no URL to reach), and without the staging data files.
        let src = testutil::offline_fixture_project();
        std::fs::remove_dir_all(src.path().join("import/staging")).unwrap();
        cmd_deploy(
            src.path().to_str().unwrap(),
            Path::new("design.yaml"),
            "dev",
            None,
            /*dry_run*/ true,
            /*no_cache*/ true,
            /*clear_cache*/ false,
            /*allow_scope_change*/ false,
            &crate::commands::OwnershipChange::default(),
            None,
            Some("incomplete_auto"),
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// A design.yaml with old-format markers, a stale internally-managed file,
    /// and a plural DDL folder all get reported (not touched) without `--fix`.
    fn write_legacy_project(dir: &std::path::Path) -> std::path::PathBuf {
        let config = dir.join("design.yaml");
        std::fs::write(
            &config,
            "project:\n  name: legacy\n  database: postgresql\nroles:\n  - name: admin\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("ddl/procedure/staging")).unwrap();
        std::fs::write(
            dir.join("ddl/procedure/staging/import_jsonb_to_table.ddl"),
            "-- stale\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("ddl/tables/public")).unwrap();
        std::fs::write(
            dir.join("ddl/tables/public/thing.ddl"),
            "create table public.thing (id int);\n",
        )
        .unwrap();
        config
    }

    #[test]
    fn doctor_reports_issues_without_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let config = write_legacy_project(tmp.path());
        let original = std::fs::read_to_string(&config).unwrap();

        cmd_doctor(
            &config,
            config.parent().unwrap(),
            None,
            /*fix*/ false,
            Verbosity::Normal,
        )
        .unwrap();

        // Report-only: nothing on disk changes.
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        assert!(
            tmp.path()
                .join("ddl/procedure/staging/import_jsonb_to_table.ddl")
                .exists()
        );
        assert!(tmp.path().join("ddl/tables").exists());
    }

    #[test]
    fn doctor_fix_migrates_backs_up_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let config = write_legacy_project(tmp.path());
        let original = std::fs::read_to_string(&config).unwrap();

        cmd_doctor(
            &config,
            config.parent().unwrap(),
            None,
            /*fix*/ true,
            Verbosity::Normal,
        )
        .unwrap();

        // Backup preserves the original content.
        let backup = tmp.path().join("design.yaml.bak");
        assert!(backup.exists(), "expected a .yaml.bak backup");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        // The migrated config is valid new-format and parses cleanly.
        dbd_core::config::read(&config).expect("migrated config should parse as DesignConfig");

        // The stale file is gone.
        assert!(
            !tmp.path()
                .join("ddl/procedure/staging/import_jsonb_to_table.ddl")
                .exists()
        );

        // The plural folder was renamed to its singular form.
        assert!(!tmp.path().join("ddl/tables").exists());
        assert!(tmp.path().join("ddl/table/public/thing.ddl").exists());
    }

    /// `dbml` writes one file per configured document when more than one is
    /// declared, using each doc's own filter/output settings.
    #[test]
    fn dbml_writes_one_file_per_configured_document() {
        let proj = testutil::copy_fixture_project();
        let cfg_path = proj.path().join("design.yaml");
        let original = std::fs::read_to_string(&cfg_path).unwrap();
        let needle = "dbml:\n  base:\n    exclude:\n      schemas:\n        - staging\n        - extensions\n";
        assert!(
            original.contains(needle),
            "fixture dbml block shape changed — update this test's patch"
        );
        let patched = original.replace(
            needle,
            "dbml:\n  base:\n    exclude:\n      schemas:\n        - staging\n        - extensions\n  extra:\n    output: extra.dbml\n    include:\n      schemas:\n        - config\n",
        );
        std::fs::write(&cfg_path, patched).unwrap();

        let out = proj.path().join("schema.dbml"); // filename ignored in the multi-doc branch; only its dir matters
        cmd_dbml(&cfg_path, "dev", proj.path(), &out, None, None, None, Verbosity::Normal).unwrap();

        assert!(proj.path().join("base.dbml").exists());
        let extra_path = proj.path().join("extra.dbml");
        assert!(extra_path.exists());
        let extra_content = std::fs::read_to_string(&extra_path).unwrap();
        assert!(
            extra_content.contains("config"),
            "extra.dbml should contain the config-schema tables"
        );
        assert!(
            !extra_content.contains("staging"),
            "extra.dbml should be filtered to the config schema only"
        );
    }

    /// A dry run says what the run does with the drops it plans (#86): with
    /// `--allow-destructive` it applies them, each named; without, it says to
    /// re-run with the flag. It said that with the flag already given.
    #[test]
    fn a_dry_run_says_what_the_run_does_with_its_drops() {
        let plan = dbd_core::ReconcilePlan {
            destructive: true,
            destructive_changes: vec!["app.t: drops column old".to_string()],
            ..altered_plan()
        };
        let allowed = reconcile_plan_lines(&plan, false, true, Verbosity::Normal).join("\n");
        assert!(!allowed.contains("re-run with --allow-destructive"), "{allowed}");
        assert!(allowed.contains("the run applies them"), "{allowed}");
        assert!(allowed.contains("app.t: drops column old"), "{allowed}");
        let refused = reconcile_plan_lines(&plan, false, false, Verbosity::Normal).join("\n");
        assert!(refused.contains("re-run with --allow-destructive"), "{refused}");
    }

    /// `reconcile_plan_lines` on an empty plan reports the in-sync message —
    /// the early-return branch the other reconcile tests never take.
    #[test]
    fn reconcile_plan_lines_empty_plan_reports_in_sync() {
        let plan = dbd_core::ReconcilePlan::default();
        let out = reconcile_plan_lines(&plan, false, false, Verbosity::Normal).join("\n");
        assert_eq!(out, "Already in sync — no changes.");
    }

    /// Added/dropped entities, warnings, and the destructive-change notice all
    /// render — dropped entities render differently depending on `--prune`.
    #[test]
    fn reconcile_plan_lines_covers_added_dropped_warnings_and_destructive() {
        let plan = dbd_core::ReconcilePlan {
            added: vec!["public.new_table".to_string()],
            matview_creates: vec!["analytics.daily_sales".to_string()],
            matview_restamps: vec!["analytics.legacy_report".to_string()],
            altered: vec![ReconcileStatement {
                entity_name: "public.users".to_string(),
                sql: "ALTER TABLE public.users ADD COLUMN email text;".to_string(),
            }],
            dropped: vec![ReconcileStatement {
                entity_name: "public.orphan".to_string(),
                sql: "DROP TABLE public.orphan CASCADE;".to_string(),
            }],
            warnings: vec!["enum value dropped".to_string()],
            destructive: true,
            ..Default::default()
        };

        let pruned = reconcile_plan_lines(&plan, true, false, Verbosity::Normal).join("\n");
        assert!(pruned.contains("+ create public.new_table"), "got: {pruned}");
        assert!(
            pruned.contains("+ create materialized view analytics.daily_sales"),
            "got: {pruned}"
        );
        assert!(
            pruned.contains("restamp materialized view analytics.legacy_report"),
            "the plan must preview a pending v1→v2 restamp; got: {pruned}"
        );
        assert!(pruned.contains("~ alter  public.users"), "got: {pruned}");
        assert!(pruned.contains("- prune  public.orphan"), "got: {pruned}");
        assert!(pruned.contains("⚠ enum value dropped"), "got: {pruned}");
        assert!(pruned.contains("--allow-destructive"), "got: {pruned}");

        let unpruned = reconcile_plan_lines(&plan, false, false, Verbosity::Normal).join("\n");
        assert!(
            unpruned.contains("· orphan public.orphan (use --prune to drop)"),
            "got: {unpruned}"
        );
    }

    /// A plan whose ONLY content is a matview drift warning must NOT report
    /// "Already in sync" — it renders the `⚠` line so real drift is surfaced
    /// rather than hidden (warnings live outside `is_empty()`).
    #[test]
    fn reconcile_plan_lines_warning_only_is_not_in_sync() {
        let plan = dbd_core::ReconcilePlan {
            warnings: vec!["materialized view app.mv: definition differs from the deployed object".to_string()],
            ..Default::default()
        };
        let out = reconcile_plan_lines(&plan, false, false, Verbosity::Normal).join("\n");
        assert!(
            !out.contains("Already in sync"),
            "a drift-only plan must not claim in-sync; got: {out}"
        );
        assert!(
            out.contains("⚠ materialized view app.mv: definition differs"),
            "the warning must render; got: {out}"
        );
    }

    /// `print_reconcile_plan` is the thin `output::always`-per-line wrapper
    /// around `reconcile_plan_lines` — just needs to run without panicking.
    #[test]
    fn print_reconcile_plan_runs_for_a_populated_plan() {
        print_reconcile_plan(&altered_plan(), false, false, Verbosity::Normal);
    }
}
