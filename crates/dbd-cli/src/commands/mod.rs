mod data;
mod diagram;
mod diff;
mod install;
mod migration;
mod project;
mod reverse;
mod schema;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dbd_core::design::{ApplyComplete, ApplyStrategy, ImportComplete};

use crate::cli::Commands;
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    command: &Commands,
    config: &Path,
    env: &str,
    database_url: Option<&str>,
    project_dir: &Path,
    source: &str,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    match command {
        Commands::Inspect { name, fix, from_db } => {
            schema::cmd_inspect(
                config,
                env,
                project_dir,
                database_url,
                name.as_deref(),
                *fix,
                *from_db,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Combine { file } => schema::cmd_combine(config, env, project_dir, file, scope, deps, verbosity),

        Commands::Refresh { name } => {
            schema::cmd_refresh(config, env, project_dir, database_url, name.as_deref(), verbosity).await
        }

        Commands::Graph { name } => {
            project::cmd_graph(config, env, project_dir, name.as_deref(), scope, deps, verbosity)
        }

        Commands::Apply {
            name,
            dry_run,
            with_policies,
            allow_scope_change,
        } => {
            schema::cmd_apply(
                config,
                env,
                project_dir,
                database_url,
                name.as_deref(),
                *dry_run,
                *with_policies,
                *allow_scope_change,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Import { name, file, dry_run } => {
            if *dry_run {
                data::cmd_import_dry_run(
                    config,
                    env,
                    project_dir,
                    name.as_deref(),
                    file.as_deref(),
                    scope,
                    deps,
                    verbosity,
                )
            } else {
                data::cmd_import(
                    config,
                    env,
                    project_dir,
                    database_url,
                    name.as_deref(),
                    file.as_deref(),
                    scope,
                    deps,
                    verbosity,
                )
                .await
            }
        }

        Commands::Reset {
            target,
            dry_run,
            force,
            schemas,
            extensions,
            clean,
            allow_scope_change,
        } => {
            let opts = migration::ResetOptions {
                dry_run: *dry_run,
                force: *force,
                drop_schemas: *schemas || *clean,
                drop_extensions: *extensions || *clean,
                allow_scope_change: *allow_scope_change,
            };
            migration::cmd_reset(
                config,
                env,
                project_dir,
                database_url,
                target,
                opts,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Snapshot { list, name } => {
            if *list {
                migration::cmd_snapshot_list(project_dir, verbosity);
                return Ok(());
            }
            migration::cmd_snapshot_create(config, env, project_dir, name.as_deref(), verbosity)
        }

        Commands::Migrate { status } => {
            if *status {
                migration::cmd_migrate_status(config, database_url, project_dir, verbosity).await
            } else {
                output::info(
                    verbosity,
                    "Use --status to check migration state. Use 'dbd apply' to run migrations.",
                );
                Ok(())
            }
        }

        Commands::Deploy {
            dry_run,
            no_cache,
            clear_cache,
            allow_scope_change,
        } => {
            project::cmd_deploy(
                source,
                config,
                env,
                database_url,
                *dry_run,
                *no_cache,
                *clear_cache,
                *allow_scope_change,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Export { name, format, output } => {
            data::cmd_export(
                config,
                env,
                project_dir,
                database_url,
                name.as_deref(),
                format,
                output.as_deref(),
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Dbml { file } => project::cmd_dbml(config, env, project_dir, file, scope, deps, verbosity),

        Commands::Diagram {
            json,
            file,
            print_url,
            site,
        } => diagram::cmd_diagram(
            config,
            env,
            project_dir,
            *json,
            file,
            *print_url,
            site.as_deref(),
            scope,
            deps,
            verbosity,
        ),

        Commands::Doctor { fix } => project::cmd_doctor(config, *fix, verbosity),

        Commands::Init {
            name,
            target,
            from_db,
            from_dbml,
            version,
            schemas,
            exclude_schemas,
            all_schemas,
            roles,
            dry_run,
        } => {
            if let Some(dbml_path) = from_dbml {
                if target != "postgres" {
                    anyhow::bail!(
                        "--target {target} is not supported with --from-dbml; \
                         reverse-engineering supports Postgres/Supabase only"
                    );
                }
                let sel = dbd_core::reverse::SchemaSelect {
                    only: schemas.clone(),
                    exclude: exclude_schemas.clone(),
                    all: *all_schemas,
                };
                return reverse::cmd_init_from_dbml(
                    project_dir,
                    dbml_path,
                    env,
                    config,
                    name.as_deref(),
                    *version,
                    sel,
                    *dry_run,
                );
            }
            if let Some(s) = from_db {
                // The reverse dialect is derived from the connection URL scheme
                // (postgres:// → postgres, sqlite://`/`file: → sqlite), so the
                // `--target` flag does not gate `--from-db`.
                let _ = target;
                let sel = dbd_core::reverse::SchemaSelect {
                    only: schemas.clone(),
                    exclude: exclude_schemas.clone(),
                    all: *all_schemas,
                };
                // Fix 1: thread database_url so the global -d flag is honoured.
                // Precedence: explicit --from-db URL > global -d/$DATABASE_URL.
                let from_db_explicit = if s.is_empty() { None } else { Some(s.as_str()) };
                reverse::cmd_init_from_db(
                    project_dir,
                    from_db_explicit,
                    database_url,
                    env,
                    config,
                    name.as_deref(),
                    *version,
                    sel,
                    *roles,
                    *dry_run,
                )
                .await
            } else {
                let project_name = name
                    .as_deref()
                    .unwrap_or_else(|| project_dir.file_name().and_then(|n| n.to_str()).unwrap_or("my-project"));
                project::cmd_init(project_dir, project_name, target, verbosity)
            }
        }

        Commands::Merge {
            conn,
            from_dbml,
            schemas,
            exclude_schemas,
            all_schemas,
            roles,
            dry_run,
        } => {
            let sel = dbd_core::reverse::SchemaSelect {
                only: schemas.clone(),
                exclude: exclude_schemas.clone(),
                all: *all_schemas,
            };
            if let Some(dbml_path) = from_dbml {
                // DBML is always foreign — no connection, no version-safety gate.
                return reverse::cmd_merge_from_dbml(project_dir, dbml_path, env, config, sel, *dry_run);
            }
            // Fix 1: thread database_url so the global -d flag is honoured.
            // Precedence: explicit positional conn > global -d/$DATABASE_URL.
            reverse::cmd_merge(
                project_dir,
                conn.as_deref(),
                database_url,
                env,
                config,
                sel,
                *roles,
                *dry_run,
            )
            .await
        }

        Commands::Format { check } => schema::cmd_format(config, project_dir, *check, verbosity),

        Commands::Policies { dry_run } => {
            schema::cmd_policies(config, env, project_dir, database_url, *dry_run, scope, deps, verbosity).await
        }

        Commands::Diff { json, exit_code } => {
            diff::cmd_diff(
                config,
                env,
                project_dir,
                database_url,
                *json,
                *exit_code,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Reconcile {
            dry_run,
            allow_destructive,
            prune,
            allow_scope_change,
        } => {
            project::cmd_reconcile(
                config,
                env,
                project_dir,
                database_url,
                *dry_run,
                *allow_destructive,
                *prune,
                *allow_scope_change,
                scope,
                deps,
                verbosity,
            )
            .await
        }

        Commands::Release { name } => project::cmd_release(config, env, project_dir, name.as_deref(), verbosity),

        Commands::Install { project, dry_run } => install::cmd_install(*project, *dry_run, project_dir, verbosity),
    }
}

// ── Summary formatting helpers ────────────────────────────────────────────────

pub(super) fn format_apply_summary(s: &ApplyComplete) -> String {
    let entities = match s.strategy {
        ApplyStrategy::Fresh => {
            format!("Fresh install at v{} — {} entities applied.", s.to_version, s.applied)
        }
        ApplyStrategy::Current => {
            format!(
                "Already up to date (v{}) — {} entities applied.",
                s.from_version, s.applied
            )
        }
        ApplyStrategy::Migrate => {
            format!(
                "Migrated v{} → v{} — {} applied, {} migrated, {} created, {} dropped.",
                s.from_version, s.to_version, s.applied, s.migrated, s.created, s.dropped
            )
        }
    };
    // Only mentioned when a project actually has hooks — a line reading
    // "0 hook script(s)" on every apply would be noise for the projects
    // (currently most of them) that declare none.
    match s.before_scripts + s.after_scripts {
        0 => entities,
        n => format!("{entities} {n} hook script(s) run."),
    }
}

pub(super) fn format_import_summary(s: &ImportComplete) -> String {
    format!(
        "Import complete — {} table(s) loaded, {} procedure(s) called, {} after script(s) run.",
        s.tables, s.procedures, s.after_scripts
    )
}

pub(super) fn format_deploy_summary(s: &dbd_core::design::DeployComplete) -> String {
    let apply_line = format_apply_summary(&s.apply);
    let import_line = format_import_summary(&s.import);
    // A scoped deploy skips policies whose table is not in this plane. That is
    // deliberate, but it must still be visible — otherwise the summary reports
    // only what ran and nothing accounts for the rest.
    let policy_line = format!(
        "{} policy file(s) applied{}{}.",
        s.policies.applied.len(),
        match s.policies.failed.len() {
            0 => String::new(),
            n => format!(", {n} failed"),
        },
        match s.policies.skipped.len() {
            0 => String::new(),
            n => format!(", {n} skipped (out of scope)"),
        }
    );
    format!("{apply_line} {import_line} {policy_line}")
}

// ── Database adapter ──────────────────────────────────────────────────────────

pub(super) async fn get_adapter(
    config: &Path,
    database_url: Option<&str>,
) -> Result<Box<dyn dbd_core::DatabaseAdapter>> {
    let design_config = dbd_core::config::read(config).context("Failed to read config")?;

    let url = match database_url {
        Some(u) => u.to_string(),
        None => {
            let target = design_config.get_target(None).context("No target configured")?;
            target
                .url
                .clone()
                .map(|u| resolve_env_vars(&u))
                .context("No database URL — set DATABASE_URL or configure target.url in design.yaml")?
        }
    };

    let project = &design_config.project.name;
    dbd_core::connect(&url, project)
        .await
        .context("Failed to connect to database")
}

fn resolve_env_vars(s: &str) -> String {
    if let Some(var) = s.strip_prefix('$') {
        std::env::var(var).unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

// ── Safe file helpers ─────────────────────────────────────────────────────────

pub(super) fn safe_canonicalize_within(root: &Path, file: &Path) -> Result<PathBuf> {
    let canon_root = root
        .canonicalize()
        .with_context(|| format!("Cannot resolve project root: {}", root.display()))?;
    // Resolve the file. If it already exists, canonicalize it directly. If it
    // doesn't (a new output file like `schema.json`), canonicalize its parent
    // directory — which must exist — and re-attach the file name, so commands
    // can create new files rather than erroring on a missing path.
    let canon_file = match file.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let file_name = file
                .file_name()
                .with_context(|| format!("Invalid output path (no file name): {}", file.display()))?;
            let parent = match file.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => root,
            };
            let canon_parent = parent
                .canonicalize()
                .with_context(|| format!("Cannot resolve directory for: {}", file.display()))?;
            canon_parent.join(file_name)
        }
    };
    anyhow::ensure!(
        canon_file.starts_with(&canon_root),
        "path traversal rejected: {} is outside project root {}",
        file.display(),
        root.display()
    );
    Ok(canon_file)
}

pub(super) fn safe_read(root: &Path, file: &Path) -> Result<String> {
    let canon = safe_canonicalize_within(root, file)?;
    std::fs::read_to_string(&canon) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to read {}", file.display()))
}

pub(super) fn safe_write(root: &Path, file: &Path, contents: &str) -> Result<()> {
    let canon = safe_canonicalize_within(root, file)?;
    std::fs::write(&canon, contents) // nosemgrep: path validated by safe_canonicalize_within
        .with_context(|| format!("Failed to write {}", file.display()))
}

pub(super) fn safe_copy(root: &Path, src: &Path, dst: &Path) -> Result<()> {
    let canon_src = safe_canonicalize_within(root, src)?;
    let parent = dst.parent().unwrap_or(root);
    let canon_parent = parent
        .canonicalize()
        .with_context(|| format!("Cannot resolve backup directory: {}", parent.display()))?;
    anyhow::ensure!(
        canon_parent.starts_with(root.canonicalize()?),
        "path traversal rejected: backup destination {} is outside project root",
        dst.display()
    );
    std::fs::copy(&canon_src, dst) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to copy {} → {}", src.display(), dst.display()))?;
    Ok(())
}

/// Shared fixtures for the per-command test modules. Read-only handlers point
/// directly at the in-repo fixture; mutating handlers (snapshot, release,
/// dbml, format) run against a throwaway copy via [`copy_fixture_project`].
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// The shared `tests/fixtures` project (`design.yaml` + `ddl/` + `import/`).
    pub(crate) fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    /// The fixture project's `design.yaml`.
    pub(crate) fn fixture_config() -> PathBuf {
        fixtures().join("design.yaml")
    }

    fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        // Test-only helper copying the compile-time fixtures tree into a tempdir — no untrusted input.
        let entries = std::fs::read_dir(src)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        for entry in entries {
            let entry = entry?;
            let to = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &to)?;
            } else {
                std::fs::copy(entry.path(), &to)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            }
        }
        Ok(())
    }

    /// Copy the fixture project into a fresh tempdir so mutating handlers never
    /// dirty the repo. The returned guard must be kept alive for the test's
    /// duration; its `path()` is the project dir and `path()/design.yaml` the
    /// config.
    pub(crate) fn copy_fixture_project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        copy_dir(&fixtures(), tmp.path()).unwrap();
        tmp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_write_creates_a_new_file() {
        // A brand-new file (doesn't exist yet) must be writable — `safe_write`
        // resolves via the parent directory rather than canonicalizing the
        // (non-existent) file itself.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("schema.json");
        assert!(!file.exists());
        safe_write(dir.path(), &file, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}");
    }

    #[test]
    fn safe_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.sql");
        std::fs::write(&file, "old").unwrap();
        safe_write(dir.path(), &file, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
    }

    #[test]
    fn safe_write_rejects_new_file_outside_root() {
        // A new file outside the project root is still rejected.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        let outside = tmp.path().join("evil.json"); // sibling of root, doesn't exist
        let err = safe_write(&root, &outside, "x").unwrap_err();
        assert!(err.to_string().contains("path traversal"), "got: {err}");
    }

    fn apply_complete(strategy: ApplyStrategy) -> ApplyComplete {
        ApplyComplete {
            strategy,
            from_version: 1,
            to_version: 2,
            applied: 3,
            migrated: 1,
            created: 2,
            dropped: 0,
            ..Default::default()
        }
    }

    /// Each apply strategy renders its own distinct summary line, and every
    /// strategy's message reflects the relevant counts (not just the label).
    #[test]
    fn format_apply_summary_covers_all_strategies() {
        let fresh = format_apply_summary(&apply_complete(ApplyStrategy::Fresh));
        assert!(fresh.contains("Fresh"));
        assert!(fresh.contains("v2"), "expected to_version in: {fresh}");
        assert!(fresh.contains("3 entities"), "expected applied count in: {fresh}");

        let current = format_apply_summary(&apply_complete(ApplyStrategy::Current));
        assert!(current.contains("up to date"));
        assert!(current.contains("v1"), "expected from_version in: {current}");
        assert!(current.contains("3 entities"), "expected applied count in: {current}");

        let migrate = format_apply_summary(&apply_complete(ApplyStrategy::Migrate));
        assert!(migrate.contains("Migrated"));
        assert!(
            migrate.contains("v1") && migrate.contains("v2"),
            "expected version range in: {migrate}"
        );
        assert!(migrate.contains("3 applied"), "expected applied count in: {migrate}");
        assert!(migrate.contains("1 migrated"), "expected migrated count in: {migrate}");
        assert!(migrate.contains("2 created"), "expected created count in: {migrate}");
        assert!(migrate.contains("0 dropped"), "expected dropped count in: {migrate}");
    }

    #[test]
    fn format_import_summary_reports_counts() {
        let s = ImportComplete {
            tables: 3,
            procedures: 2,
            after_scripts: 1,
            ..Default::default()
        };
        let out = format_import_summary(&s);
        assert!(out.contains("3 table"), "expected table count in: {out}");
        assert!(out.contains("2 procedure"), "expected procedure count in: {out}");
        assert!(out.contains("1 after script"), "expected after-script count in: {out}");
    }

    /// An import that loaded nothing still reports — a blank line here is what
    /// made a deploy that seeded no rows look like a success.
    #[test]
    fn format_import_summary_reports_zero_explicitly() {
        let out = format_import_summary(&ImportComplete::default());
        assert!(out.contains("0 table"), "a zero import must still be stated: {out}");
    }

    #[test]
    fn format_deploy_summary_combines_apply_import_and_policies() {
        let s = dbd_core::design::DeployComplete {
            apply: apply_complete(ApplyStrategy::Fresh),
            import: ImportComplete {
                tables: 2,
                procedures: 1,
                ..Default::default()
            },
            policies: dbd_core::design::PolicyReport {
                applied: vec![std::path::PathBuf::from("policies/users.sql")],
                failed: Vec::new(),
                skipped: Vec::new(),
            },
        };
        let out = format_deploy_summary(&s);
        assert!(out.contains("Fresh") && out.contains("table"));
        assert!(
            out.contains("1 policy file(s) applied"),
            "policy phase must be reported: {out}"
        );
    }

    /// Failed policy files are non-fatal, so the summary must say how many
    /// failed — otherwise a deploy reports success with RLS not applied.
    #[test]
    fn format_deploy_summary_reports_failed_policies() {
        let s = dbd_core::design::DeployComplete {
            policies: dbd_core::design::PolicyReport {
                applied: Vec::new(),
                skipped: Vec::new(),
                failed: vec![(std::path::PathBuf::from("policies/bad.sql"), "boom".to_string())],
            },
            ..Default::default()
        };
        let out = format_deploy_summary(&s);
        assert!(out.contains("1 failed"), "failed policy count must be reported: {out}");
    }

    /// A scoped deploy skips policies whose table is not in the plane. The
    /// `apply` and `policies` paths already print each skip; the deploy summary
    /// must count them too, or `deploy --scope` reports "2 applied" on a project
    /// with 5 policy files and nothing says the other 3 were deliberate.
    #[test]
    fn format_deploy_summary_reports_skipped_policies() {
        let s = dbd_core::design::DeployComplete {
            policies: dbd_core::design::PolicyReport {
                applied: vec![std::path::PathBuf::from("policies/app/t.sql")],
                failed: Vec::new(),
                skipped: vec![(
                    std::path::PathBuf::from("policies/dojo/repository_metrics.sql"),
                    "dojo.repository_metrics is outside scope 'daemon'".to_string(),
                )],
            },
            ..Default::default()
        };
        let out = format_deploy_summary(&s);
        assert!(
            out.contains("1 skipped"),
            "skipped policy count must be reported: {out}"
        );
    }

    /// A skip is not a failure: with nothing skipped the summary must not
    /// mention skipping at all, so the phrase stays a real signal.
    #[test]
    fn format_deploy_summary_omits_skipped_when_none() {
        let s = dbd_core::design::DeployComplete::default();
        let out = format_deploy_summary(&s);
        assert!(!out.contains("skipped"), "must not mention skipping when none: {out}");
    }

    /// `$VAR` is looked up (falling back to the literal when unset); a plain
    /// value passes through untouched.
    #[test]
    fn resolve_env_vars_passthrough_and_fallback() {
        assert_eq!(resolve_env_vars("postgres://x/db"), "postgres://x/db");
        assert_eq!(
            resolve_env_vars("$definitely_unset_var_xyz"),
            "$definitely_unset_var_xyz"
        );
    }

    /// The `run` dispatcher routes a non-DB command (Doctor) to its handler.
    #[tokio::test]
    async fn run_dispatches_non_db_command() {
        use crate::cli::Commands;
        use crate::commands::testutil;
        let cmd = Commands::Doctor { fix: false };
        let fixtures = testutil::fixtures();
        run(
            &cmd,
            &testutil::fixture_config(),
            "dev",
            None,
            &fixtures,
            fixtures.to_str().unwrap(),
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    // ── `run` dispatch ────────────────────────────────────────────────────────
    //
    // The dispatcher is pure delegation, so these assert the *routing*: each
    // command reaches a handler that produces its own observable effect (a file
    // written, a specific error). Read-only commands run against the in-repo
    // fixture; anything that writes runs against a throwaway copy.

    /// Dispatch `cmd` against a throwaway copy of the fixture project, returning
    /// the result and keeping the temp dir alive for the caller's assertions.
    async fn run_in_copy(cmd: &crate::cli::Commands, tmp: &tempfile::TempDir) -> Result<()> {
        let dir = tmp.path();
        run(
            cmd,
            &dir.join("design.yaml"),
            "dev",
            None,
            dir,
            dir.to_str().unwrap(),
            None,
            None,
            Verbosity::Normal,
        )
        .await
    }

    /// `dbd combine` writes the concatenated schema to the requested file.
    #[tokio::test]
    async fn run_dispatches_combine_and_writes_the_file() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        let out = tmp.path().join("combined.sql");
        run_in_copy(&Commands::Combine { file: out.clone() }, &tmp)
            .await
            .unwrap();
        let written = std::fs::read_to_string(&out).expect("combine should write the file");
        assert!(
            written.to_lowercase().contains("create table"),
            "combined output should contain the project's DDL"
        );
    }

    /// `dbd dbml` writes a DBML document.
    #[tokio::test]
    async fn run_dispatches_dbml_and_writes_the_file() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        let out = tmp.path().join("schema.dbml");
        run_in_copy(&Commands::Dbml { file: out.clone() }, &tmp).await.unwrap();
        let written = std::fs::read_to_string(&out).expect("dbml should write the file");
        assert!(written.contains("table"), "expected DBML table blocks: {written:.120}");
    }

    /// `dbd diagram --json` writes the diagram model.
    #[tokio::test]
    async fn run_dispatches_diagram_json_and_writes_the_file() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        let out = tmp.path().join("diagram.json");
        run_in_copy(
            &Commands::Diagram {
                json: true,
                file: out.clone(),
                print_url: false,
                site: None,
            },
            &tmp,
        )
        .await
        .unwrap();
        let written = std::fs::read_to_string(&out).expect("diagram should write the file");
        assert!(
            serde_json::from_str::<serde_json::Value>(&written).is_ok(),
            "diagram --json must write valid JSON"
        );
    }

    /// `dbd snapshot` creates a snapshot; `dbd snapshot --list` takes the
    /// early-return listing branch.
    #[tokio::test]
    async fn run_dispatches_snapshot_create_then_list() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(
            &Commands::Snapshot {
                list: false,
                name: Some("via run".into()),
            },
            &tmp,
        )
        .await
        .unwrap();
        assert!(
            !dbd_core::snapshot::list_snapshots(tmp.path()).is_empty(),
            "snapshot dispatch should have created one"
        );
        run_in_copy(&Commands::Snapshot { list: true, name: None }, &tmp)
            .await
            .unwrap();
    }

    /// `dbd import --dry-run` routes to the dry-run handler, not the DB one —
    /// proven by it succeeding with no database configured.
    #[tokio::test]
    async fn run_dispatches_import_dry_run_without_a_database() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(
            &Commands::Import {
                name: None,
                file: None,
                dry_run: true,
            },
            &tmp,
        )
        .await
        .unwrap();
    }

    /// `dbd reset --dry-run` likewise returns before any adapter is built.
    #[tokio::test]
    async fn run_dispatches_reset_dry_run_without_a_database() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(
            &Commands::Reset {
                target: "dev".to_string(),
                dry_run: true,
                force: false,
                schemas: false,
                extensions: false,
                clean: false,
                allow_scope_change: false,
            },
            &tmp,
        )
        .await
        .unwrap();
    }

    /// `dbd migrate` without `--status` is the informational branch: it prints
    /// guidance and succeeds without touching a database.
    #[tokio::test]
    async fn run_dispatches_migrate_without_status_needs_no_database() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(&Commands::Migrate { status: false }, &tmp).await.unwrap();
    }

    /// `dbd format` routes to the formatter and rewrites the project's DDL.
    ///
    /// Deliberately not `--check`: that path calls `std::process::exit(1)` when
    /// files would change (`schema::cmd_format`), which would terminate the test
    /// harness rather than fail a test.
    #[tokio::test]
    async fn run_dispatches_format_and_rewrites_ddl() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        let target = tmp.path().join("ddl/table/config/lookups.ddl");
        let before = std::fs::read_to_string(&target).unwrap();

        run_in_copy(&Commands::Format { check: false }, &tmp).await.unwrap();

        let after = std::fs::read_to_string(&target).unwrap();
        assert_ne!(
            before, after,
            "the fixture DDL is unformatted, so `format` should have rewritten it"
        );
    }

    /// `dbd graph` routes to the graph handler and succeeds offline.
    #[tokio::test]
    async fn run_dispatches_graph() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(&Commands::Graph { name: None }, &tmp).await.unwrap();
    }

    /// `dbd install --dry-run` reports what it would write without writing.
    #[tokio::test]
    async fn run_dispatches_install_dry_run() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        run_in_copy(
            &Commands::Install {
                project: true,
                dry_run: true,
            },
            &tmp,
        )
        .await
        .unwrap();
    }

    /// `dbd init --from-dbml` with a non-Postgres target is rejected in the
    /// dispatcher itself, before any reverse-engineering runs.
    #[tokio::test]
    async fn run_rejects_init_from_dbml_for_non_postgres_target() {
        use crate::cli::Commands;
        let tmp = testutil::copy_fixture_project();
        let err = run_in_copy(
            &Commands::Init {
                name: Some("x".to_string()),
                target: "sqlite".to_string(),
                from_db: None,
                from_dbml: Some(tmp.path().join("schema.dbml")),
                version: 1,
                schemas: Vec::new(),
                exclude_schemas: Vec::new(),
                all_schemas: false,
                roles: false,
                dry_run: true,
            },
            &tmp,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("--from-dbml"),
            "the guard must name the offending flag: {err}"
        );
    }

    /// Plain `dbd init` (no `--from-db`/`--from-dbml`) scaffolds a project in an
    /// empty directory.
    #[tokio::test]
    async fn run_dispatches_plain_init_and_scaffolds_a_project() {
        use crate::cli::Commands;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(
            &Commands::Init {
                name: Some("scaffolded".to_string()),
                target: "postgres".to_string(),
                from_db: None,
                from_dbml: None,
                version: 1,
                schemas: Vec::new(),
                exclude_schemas: Vec::new(),
                all_schemas: false,
                roles: false,
                dry_run: false,
            },
            &dir.join("design.yaml"),
            "dev",
            None,
            dir,
            dir.to_str().unwrap(),
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();

        let cfg = dir.join("design.yaml");
        assert!(cfg.exists(), "init should have written design.yaml");
        let written = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            written.contains("scaffolded"),
            "the project name should reach design.yaml: {written}"
        );
    }

    /// `dbd init --from-dbml --dry-run` routes through the reverse-engineering
    /// handler. The DBML input is produced by `dbd dbml` on the fixture, so the
    /// two commands are exercised against each other rather than a stale file.
    #[tokio::test]
    async fn run_dispatches_init_from_dbml_dry_run() {
        use crate::cli::Commands;
        let src = testutil::copy_fixture_project();
        let dbml = src.path().join("schema.dbml");
        run_in_copy(&Commands::Dbml { file: dbml.clone() }, &src).await.unwrap();
        assert!(dbml.exists(), "precondition: DBML input must exist");

        let dest = tempfile::tempdir().unwrap();
        let dir = dest.path();
        run(
            &Commands::Init {
                name: Some("from_dbml".to_string()),
                target: "postgres".to_string(),
                from_db: None,
                from_dbml: Some(dbml),
                version: 1,
                schemas: Vec::new(),
                exclude_schemas: Vec::new(),
                all_schemas: false,
                roles: false,
                dry_run: true,
            },
            &dir.join("design.yaml"),
            "dev",
            None,
            dir,
            dir.to_str().unwrap(),
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();

        assert!(
            !dir.join("design.yaml").exists(),
            "--dry-run must not write design.yaml"
        );
    }

    /// `dbd merge --from-dbml --dry-run` takes the no-connection merge branch.
    #[tokio::test]
    async fn run_dispatches_merge_from_dbml_dry_run() {
        use crate::cli::Commands;
        let proj = testutil::copy_fixture_project();
        let dbml = proj.path().join("schema.dbml");
        run_in_copy(&Commands::Dbml { file: dbml.clone() }, &proj)
            .await
            .unwrap();

        run_in_copy(
            &Commands::Merge {
                conn: None,
                from_dbml: Some(dbml),
                schemas: Vec::new(),
                exclude_schemas: Vec::new(),
                all_schemas: false,
                roles: false,
                dry_run: true,
            },
            &proj,
        )
        .await
        .unwrap();
    }

    /// `dbd release` routes to the release handler.
    #[tokio::test]
    async fn run_dispatches_release() {
        use crate::cli::Commands;
        let proj = testutil::copy_fixture_project();
        run_in_copy(
            &Commands::Release {
                name: Some("v1".to_string()),
            },
            &proj,
        )
        .await
        .unwrap();
    }

    // ── get_adapter ───────────────────────────────────────────────────────────

    /// A config with no reachable URL fails before any connection is attempted,
    /// with a message pointing at how to supply one.
    #[tokio::test]
    async fn get_adapter_without_a_url_explains_how_to_set_one() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("design.yaml");
        std::fs::write(
            &cfg,
            "project:\n  name: nourl\nsource:\n  dialect: postgresql\n\
             target:\n  postgres: {}\nschemas:\n  - app\n",
        )
        .unwrap();

        let err = get_adapter(&cfg, None).await.err().expect("expected an error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("DATABASE_URL") || msg.contains("target.url") || msg.contains("No target"),
            "expected an actionable message about the missing URL, got: {msg}"
        );
    }

    /// An unreadable config is reported as a config error, not a connection one.
    #[tokio::test]
    async fn get_adapter_reports_unreadable_config() {
        let err = get_adapter(Path::new("/nonexistent/design.yaml"), None)
            .await
            .err()
            .expect("expected an error");
        let msg = format!("{err:#}");
        assert!(msg.contains("config"), "expected a config-read error, got: {msg}");
    }

    /// `$VAR` in `target.url` is resolved from the environment by `get_adapter`.
    /// Uses a bogus host so it fails at connect time — proving the variable was
    /// expanded rather than passed through literally.
    #[tokio::test]
    async fn get_adapter_expands_env_var_in_target_url() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("design.yaml");
        std::fs::write(
            &cfg,
            "project:\n  name: envurl\nsource:\n  dialect: postgresql\n\
             target:\n  postgres:\n    url: $DBD_TEST_URL_VAR\nschemas:\n  - app\n",
        )
        .unwrap();
        // SAFETY: single-threaded test setup; the var is unique to this test.
        unsafe {
            std::env::set_var("DBD_TEST_URL_VAR", "postgres://user@127.0.0.1:1/nope");
        }

        let err = get_adapter(&cfg, None).await.err().expect("expected an error");
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("$DBD_TEST_URL_VAR"),
            "the env var should have been expanded, not passed through: {msg}"
        );

        unsafe {
            std::env::remove_var("DBD_TEST_URL_VAR");
        }
    }
}
