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
//!
//! `#[cfg(test)]` on every function below is temporary, not a lint dodge: this
//! module lands ahead of its own pipeline wiring (a deliberate two-step split,
//! so the config type and this resolution logic get their own review before
//! `apply.rs`/`import.rs` call into them), so right now nothing but the tests
//! calls them. Marking that explicitly — rather than `pub(crate)` with no
//! caller, which this repo's pre-commit hook rejects as dead code — says so
//! truthfully instead of suppressing the lint. Delete every `#[cfg(test)]` here
//! the moment a real caller lands.

#[cfg(test)]
use crate::config::ScriptEntry;

/// Tables a script references, derived from its SQL.
///
/// Empty when the SQL cannot be parsed, or when its table names are data rather
/// than identifiers. Callers treat empty as "unknowable, so run it": silently
/// skipping a hook because analysis came up short would hide, for instance, a
/// realtime hook quietly not firing.
#[cfg(test)]
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
#[cfg(test)]
pub(crate) fn hook_in_scope(
    deps: &[String],
    working_set: &std::collections::HashSet<String>,
    is_all: bool,
) -> bool {
    if is_all || deps.is_empty() {
        return true;
    }
    deps.iter().all(|d| working_set.contains(d))
}

/// A hook's dependencies: declared if present, derived otherwise.
#[cfg(test)]
pub(crate) fn dependencies_of(entry: &ScriptEntry, sql: &str) -> Vec<String> {
    match entry.declared_writes() {
        Some(w) => w.to_vec(),
        None => derive_dependencies(sql),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ws(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
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
