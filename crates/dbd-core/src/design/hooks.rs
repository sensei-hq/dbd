//! Lifecycle hook scripts: dependency resolution and scope filtering.
//!
//! A hook runs when every table it depends on is in the working set — the same
//! rule `import_entry_in_scope` applies to staging entries, so a scoped run
//! never executes a loader against half-loaded data.
//!
//! Dependencies come from the script's SQL where they can, and from an explicit
//! `writes:` where they cannot. That second case is not exotic: a script naming
//! its tables inside `format()` or an `array[…]` — the shape of a real
//! realtime-publication hook, measured — is opaque to every parser. `writes:`
//! is the primary mechanism for that shape, not an escape hatch for it.

use std::collections::HashSet;
use std::path::Path;

use crate::adapter::DatabaseAdapter;
use crate::config::ScriptEntry;
use crate::error::{DbdError, Result};

/// Tables a script references, derived from its SQL.
///
/// Empty when the SQL cannot be parsed, or when its table names are data rather
/// than identifiers. Callers treat empty as "unknowable, so run it": silently
/// skipping a hook because analysis came up short would hide, for instance, a
/// realtime hook quietly not firing.
pub(crate) fn derive_dependencies(sql: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let default_schema = crate::parser::pg::common::extract_search_paths_via_pg_query(sql)
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    let mut out: Vec<String> = Vec::new();
    for name in parsed.tables() {
        if let Some(q) = crate::parser::pg::common::qualify_name_str(&name, &default_schema)
            && !out.contains(&q)
        {
            out.push(q);
        }
    }
    out.sort();
    out
}

/// Whether a hook's dependencies are satisfied by the working set.
///
/// `is_all` short-circuits before any derivation cost — the common case, an
/// unscoped run, always runs every hook. An empty `deps` (derivation found
/// nothing) also always runs: see [`derive_dependencies`]'s doc comment for why
/// that must never mean "skip".
pub(crate) fn hook_in_scope(deps: &[String], working_set: &HashSet<String>, is_all: bool) -> bool {
    if is_all || deps.is_empty() {
        return true;
    }
    deps.iter().all(|d| working_set.contains(d))
}

/// A hook's dependencies: declared if present, derived otherwise.
pub(crate) fn dependencies_of(entry: &ScriptEntry, sql: &str) -> Vec<String> {
    match entry.declared_writes() {
        Some(w) => w.to_vec(),
        None => derive_dependencies(sql),
    }
}

/// Which lifecycle phase a hook list belongs to, so a skip warning says which
/// of a project's two hook points went quiet.
#[derive(Debug, Clone, Copy)]
pub(in crate::design) enum HookKind {
    Before,
    After,
}

impl HookKind {
    fn label(self) -> &'static str {
        match self {
            Self::Before => "before-script",
            Self::After => "after-script",
        }
    }
}

/// What one phase's hook list did: how many scripts ran, and why any did not.
#[derive(Debug, Default)]
pub(in crate::design) struct HookOutcome {
    pub ran: u32,
    /// One line per skipped hook, for the caller's `warnings` channel. A skip is
    /// never fatal — a scoped run is a normal workflow — but it is never silent
    /// either.
    pub warnings: Vec<String>,
}

/// What one phase's hook list *would* do, decided before anything executes.
#[derive(Debug, Default)]
pub(in crate::design) struct HookPlan {
    /// Each hook that will run, as `(declared path, its SQL)`, in list order.
    pub runnable: Vec<(String, String)>,
    /// One line per hook a scope filtered out.
    pub warnings: Vec<String>,
}

/// Decide a hook list without executing any of it: read every script, resolve
/// each one's dependencies, and partition into "will run" and "skipped, here is
/// why".
///
/// Separated from [`run_hooks`] so a preview and the run it previews reach the
/// same verdict from the same code — `dbd import --dry-run` listing a script
/// the real run then skips would be its own small version of the silent
/// disagreement this feature exists to remove.
///
/// Every file is read up front, so a hook the config names but the filesystem
/// does not have aborts the phase before any *other* hook has run. That a
/// missing script is an error at all is deliberate: dbd cannot tell a deleted
/// file from a mistyped path, and continuing past either is how a declared hook
/// comes to do nothing without saying so.
pub(in crate::design) fn plan_hooks(
    project_dir: &Path,
    entries: &[ScriptEntry],
    kind: HookKind,
    scope: Option<(&str, &HashSet<String>)>,
) -> Result<HookPlan> {
    // Canonicalize the root once so the containment check is reliable, matching
    // how `apply_policies` guards the files it executes.
    let canon_root = project_dir.canonicalize().unwrap_or_else(|_| project_dir.to_path_buf());

    let mut plan = HookPlan::default();
    for entry in entries {
        let script = entry.script();
        let sql = read_hook(&canon_root, script, kind)?;

        if let Some((scope_name, working_set)) = scope {
            let deps = dependencies_of(entry, &sql);
            if !hook_in_scope(&deps, working_set, false) {
                let missing: Vec<&str> = deps
                    .iter()
                    .filter(|d| !working_set.contains(*d))
                    .map(String::as_str)
                    .collect();
                plan.warnings.push(format!(
                    "{} {script} skipped — needs {}, which is outside scope '{scope_name}'",
                    kind.label(),
                    missing.join(", ")
                ));
                continue;
            }
        }
        plan.runnable.push((script.to_string(), sql));
    }
    Ok(plan)
}

/// A declared hook path rebuilt from ordinary path segments alone, or `None` if
/// it contains anything else.
///
/// Rebuilding rather than inspecting-and-passing-through means the value that
/// reaches the filesystem is constructed from components this function has
/// individually accepted, so there is no route by which an unexamined byte of
/// the original string becomes part of a path.
fn safe_relative_path(script: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    let mut any = false;
    for component in Path::new(script).components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                any = true;
            }
            // `./foo` is a harmless way to write `foo`.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    any.then_some(out)
}

/// Read a hook script, refusing one that resolves outside the project.
///
/// `dbd deploy <github-source>` runs a design.yaml dbd downloaded, so a hook
/// path is not necessarily the local operator's own text — `../../.ssh/id_rsa`
/// in one would otherwise be handed straight to the database. Same containment
/// rule `apply_policies` applies to `policies/`.
fn read_hook(canon_root: &Path, script: &str, kind: HookKind) -> Result<String> {
    let denied = |reason: String| DbdError::Config(format!("{} {script} could not be read: {reason}", kind.label()));

    // Rebuild the path from plain names only, rather than trusting the declared
    // string and judging the result: a hook is always project-relative, so
    // anything that is not an ordinary path segment — `..`, a leading `/`, a
    // Windows drive prefix — is refused here and never reaches a join.
    let relative = safe_relative_path(script)
        .ok_or_else(|| denied("hook paths must be relative to the project, with no '..'".to_string()))?;

    // Resolve and re-check: `..` is not the only way out of the project — a
    // symlink under it points wherever it likes.
    let path = canon_root
        .join(relative)
        .canonicalize()
        .map_err(|e| denied(format!("{}/{script} — {e}", canon_root.display())))?;
    if !path.starts_with(canon_root) {
        return Err(denied(format!(
            "it resolves outside {} — refusing to run it",
            canon_root.display()
        )));
    }
    std::fs::read_to_string(&path).map_err(|e| denied(e.to_string()))
}

/// Run one phase's hook scripts, honouring scope and `dry_run`.
///
/// `scope` is `Some((name, working_set))` for a narrowed run and `None` for an
/// unscoped or all-scope one, where every hook runs without paying for
/// derivation.
///
/// [`plan_hooks`] makes every decision; this only executes it, so `dry_run`
/// reports the identical set of steps while running none of them — the shape
/// `import_run_after_scripts` already had.
pub(in crate::design) async fn run_hooks<S, D, C>(
    adapter: &dyn DatabaseAdapter,
    project_dir: &Path,
    entries: &[ScriptEntry],
    kind: HookKind,
    scope: Option<(&str, &HashSet<String>)>,
    dry_run: bool,
    progress: &mut super::Progress<S, D, C>,
) -> Result<HookOutcome>
where
    S: FnMut(&str),
    D: FnMut(&str, Option<&str>),
{
    let plan = plan_hooks(project_dir, entries, kind, scope)?;
    let mut outcome = HookOutcome {
        ran: 0,
        warnings: plan.warnings,
    };
    for (script, sql) in &plan.runnable {
        let desc = format!("run {script}");
        (progress.on_start)(&desc);
        let result = if dry_run {
            Ok(())
        } else {
            adapter.execute_script(sql).await
        };
        super::report_step_result(&desc, &mut progress.on_done, result)?;
        outcome.ran += 1;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::mock::MockAdapter;
    use crate::design::Progress;
    use std::collections::HashSet;

    fn ws(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A progress sink for the tests that assert on the outcome, not the steps.
    type SilentProgress = Progress<fn(&str), fn(&str, Option<&str>), fn(())>;
    fn silent() -> SilentProgress {
        Progress::none()
    }

    /// A throwaway project directory holding one hook script.
    fn project_with_script(rel: &str, sql: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, sql).unwrap();
        tmp
    }

    /// A hook the user declared and dbd cannot find is a misconfiguration, not
    /// an optional step — running the rest of the phase as if nothing were
    /// missing is exactly the silent skip this feature exists to remove.
    #[tokio::test]
    async fn a_missing_hook_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::Path("sql/absent.sql".to_string())];
        let err = run_hooks(&mock, tmp.path(), &entries, HookKind::After, None, false, &mut silent())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sql/absent.sql"), "the error must name the script: {msg}");
    }

    /// `dbd deploy <github-source>` executes a design.yaml dbd downloaded, so a
    /// hook path is not always the local operator's own text. One escaping the
    /// project is refused rather than read.
    #[tokio::test]
    async fn a_hook_path_escaping_the_project_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("secret.sql"), "select 'pwned';").unwrap();
        let project = outer.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::Path("../secret.sql".to_string())];
        let err = run_hooks(&mock, &project, &entries, HookKind::After, None, false, &mut silent())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("relative"), "got: {err}");
        assert_eq!(mock.script_count(), 0, "nothing outside the project may execute");
    }

    /// Rebuilding the path keeps the harmless spellings working while refusing
    /// every way out of the project.
    #[test]
    fn only_ordinary_path_segments_survive_rebuilding() {
        assert_eq!(safe_relative_path("./sql/hook.sql"), Some("sql/hook.sql".into()));
        assert_eq!(safe_relative_path("sql/hook.sql"), Some("sql/hook.sql".into()));
        assert_eq!(safe_relative_path("../escape.sql"), None);
        assert_eq!(safe_relative_path("sql/../../escape.sql"), None);
        assert_eq!(safe_relative_path("/etc/passwd"), None);
        assert_eq!(safe_relative_path(""), None);
    }

    /// `--dry-run` reports what would run without executing it, matching the
    /// shape `import_run_after_scripts` already had.
    #[tokio::test]
    async fn a_dry_run_reports_without_executing() {
        let tmp = project_with_script("sql/hook.sql", "select 1;");
        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::Path("sql/hook.sql".to_string())];
        let mut reported: Vec<String> = Vec::new();
        let outcome = run_hooks(
            &mock,
            tmp.path(),
            &entries,
            HookKind::After,
            None,
            /*dry_run*/ true,
            &mut Progress {
                on_start: |d: &str| reported.push(d.to_string()),
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |_: ()| {},
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome.ran, 1, "a dry run still reports the hook it would run");
        assert_eq!(mock.script_count(), 0, "a dry run must execute nothing");
        assert!(reported.iter().any(|d| d.contains("sql/hook.sql")), "got {reported:?}");
    }

    /// The whole feature in one assertion: a derivable hook whose table the
    /// scope excludes is skipped, and the warning names both the script and the
    /// table that put it out of scope.
    #[tokio::test]
    async fn an_out_of_scope_hook_is_skipped_and_the_warning_names_the_table() {
        let tmp = project_with_script(
            "sql/loader.sql",
            "insert into app.target select * from staging.a join staging.b on true;",
        );
        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::Path("sql/loader.sql".to_string())];
        let working = ws(&["app.target", "staging.a"]);
        let outcome = run_hooks(
            &mock,
            tmp.path(),
            &entries,
            HookKind::After,
            Some(("partial", &working)),
            false,
            &mut silent(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.ran, 0, "the hook must not run");
        assert_eq!(mock.script_count(), 0);
        let warning = outcome.warnings.first().expect("a skip must be warned about");
        assert!(warning.contains("sql/loader.sql"), "got: {warning}");
        assert!(
            warning.contains("staging.b"),
            "the offending table must be named: {warning}"
        );
        assert!(warning.contains("partial"), "the scope must be named: {warning}");
    }

    /// The `sensei` shape: a script whose table names are data derives nothing,
    /// so its declared `writes:` is the only thing scope filtering can use — and
    /// with those writes in scope, it runs.
    #[tokio::test]
    async fn a_declared_writes_hook_runs_when_its_writes_are_in_scope() {
        let tmp = project_with_script(
            "sql/dynamic.sql",
            "do $$ begin execute format('insert into %I.target values (1)', 'app'); end $$;",
        );
        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::WithWrites {
            script: "sql/dynamic.sql".to_string(),
            writes: vec!["app.target".to_string()],
        }];
        let working = ws(&["app.target", "staging.a"]);
        let outcome = run_hooks(
            &mock,
            tmp.path(),
            &entries,
            HookKind::After,
            Some(("partial", &working)),
            false,
            &mut silent(),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.ran, 1,
            "declared writes in scope must run: {:?}",
            outcome.warnings
        );
        assert_eq!(mock.script_count(), 1);
    }

    /// A `before` hook says so in its warning — the two phases must be
    /// distinguishable in the output, or a user cannot tell which one skipped.
    #[tokio::test]
    async fn a_before_hook_labels_itself_as_one() {
        let tmp = project_with_script("sql/pre.sql", "insert into staging.b values (1);");
        let mock = MockAdapter::new();
        let entries = vec![ScriptEntry::Path("sql/pre.sql".to_string())];
        let working = ws(&["staging.a"]);
        let outcome = run_hooks(
            &mock,
            tmp.path(),
            &entries,
            HookKind::Before,
            Some(("partial", &working)),
            false,
            &mut silent(),
        )
        .await
        .unwrap();
        let warning = outcome.warnings.first().expect("a skip must be warned about");
        assert!(warning.starts_with("before-script "), "got: {warning}");
    }

    #[test]
    fn a_script_whose_tables_are_all_in_scope_runs() {
        let deps = vec!["staging.a".to_string(), "staging.b".to_string()];
        assert!(hook_in_scope(&deps, &ws(&["staging.a", "staging.b"]), false));
    }

    #[test]
    fn a_script_missing_one_table_is_skipped() {
        let deps = vec!["staging.a".to_string(), "staging.b".to_string()];
        assert!(!hook_in_scope(&deps, &ws(&["staging.a"]), false));
    }

    /// Derivation found nothing — the script's dependencies are unknowable, so
    /// it runs. Skipping silently would hide, say, a realtime hook not firing.
    #[test]
    fn a_script_with_no_derivable_dependencies_runs() {
        assert!(hook_in_scope(&[], &ws(&["staging.a"]), false));
    }

    /// The all-scope short-circuits before any derivation cost.
    #[test]
    fn the_all_scope_runs_everything() {
        let deps = vec!["nothing.matching".to_string()];
        assert!(hook_in_scope(&deps, &ws(&[]), true));
    }

    #[test]
    fn plain_sql_dependencies_are_derived_and_qualified() {
        let deps = derive_dependencies(
            "set search_path to app;\ninsert into target select * from staging.a join b on b.id = a.id;",
        );
        assert!(deps.contains(&"app.target".to_string()), "got {deps:?}");
        assert!(deps.contains(&"staging.a".to_string()), "got {deps:?}");
        assert!(deps.contains(&"app.b".to_string()), "got {deps:?}");
    }

    /// Measured against sensei's real realtime hook: its table names live in
    /// `array[…]` and a `format()` string, so no parser can see them. This is
    /// the case `writes:` exists for.
    #[test]
    fn a_do_block_naming_tables_in_data_derives_nothing() {
        let deps = derive_dependencies(
            "do $$ begin\n  execute format('alter publication p add table dojo.%I', 'x');\nend $$;",
        );
        assert!(deps.is_empty(), "expected no derivable deps, got {deps:?}");
    }

    #[test]
    fn unparseable_sql_derives_nothing_rather_than_erroring() {
        assert!(derive_dependencies("NOT SQL AT ALL ;;;").is_empty());
    }

    #[test]
    fn declared_writes_win_over_derivation() {
        let entry = ScriptEntry::WithWrites {
            script: "sql/dyn.sql".to_string(),
            writes: vec!["app.target".to_string()],
        };
        // The SQL body derives nothing on its own (it's the do-block shape), but
        // the declared `writes:` still resolves — that's the whole point of the
        // object form.
        let deps = dependencies_of(
            &entry,
            "do $$ begin execute format('insert into %I.target values (1)', 'app'); end $$;",
        );
        assert_eq!(deps, vec!["app.target".to_string()]);
    }

    #[test]
    fn no_declared_writes_falls_back_to_derivation() {
        let entry = ScriptEntry::Path("sql/loader.sql".to_string());
        let deps = dependencies_of(&entry, "insert into app.target select * from staging.a;");
        assert!(deps.contains(&"app.target".to_string()), "got {deps:?}");
        assert!(deps.contains(&"staging.a".to_string()), "got {deps:?}");
    }
}
