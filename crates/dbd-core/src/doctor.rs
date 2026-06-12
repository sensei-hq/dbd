use std::path::{Path, PathBuf};

use crate::error::{DbdError, Result};

/// A stale file in the project that dbd now manages internally.
#[derive(Debug, Clone)]
pub struct StaleFile {
    pub path: PathBuf,
    pub reason: String,
}

/// Files that dbd previously scaffolded into user projects but now manages
/// internally. Old projects may still have these; they should be removed.
const MANAGED_INTERNALLY: &[(&str, &str)] = &[(
    "ddl/procedure/staging/import_jsonb_to_table.ddl",
    "now managed internally by dbd — apply via ensure_import_procedure(), remove from project DDL",
)];

/// Scan the project directory for files that were once user-scaffolded but are
/// now managed internally by dbd. Returns one entry per stale file found.
pub fn detect_stale_files(project_dir: &Path) -> Vec<StaleFile> {
    MANAGED_INTERNALLY
        .iter()
        .filter_map(|(rel_path, reason)| {
            let full = project_dir.join(rel_path);
            if full.exists() {
                Some(StaleFile {
                    path: full,
                    reason: reason.to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Remove stale internally-managed files from the project directory.
/// Returns the list of files that were successfully removed.
pub fn remove_stale_files(stale: &[StaleFile]) -> Vec<(PathBuf, Option<String>)> {
    stale
        .iter()
        .map(|f| match std::fs::remove_file(&f.path) {
            Ok(()) => (f.path.clone(), None),
            Err(e) => (f.path.clone(), Some(e.to_string())),
        })
        .collect()
}

/// Detect whether a design.yaml is in the old (Node.js) format.
///
/// Old format indicators:
/// - `project.database` exists (should be `source.dialect` + `target.<name>`)
/// - Top-level `extensions` key
/// - Top-level `roles` key
/// - `import.options.nullValue` (camelCase, should be `null_value`)
/// - `project.staging` (should be `import.staging`)
/// - `project.dbdocs` (should be top-level `dbml`)
pub fn detect_old_format(content: &str) -> Vec<String> {
    let doc: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return vec!["Failed to parse YAML".to_string()],
    };

    let mut issues = Vec::new();

    // Check for old-format indicators
    if let Some(project) = doc.get("project") {
        if project.get("database").is_some() {
            issues.push("project.database → move to source.dialect + target.<name>".to_string());
        }
        if project.get("staging").is_some() {
            issues.push("project.staging → move to import.staging".to_string());
        }
        if project.get("extensionSchema").is_some() {
            issues.push("project.extensionSchema → use per-extension schema in target".to_string());
        }
        if project.get("dbdocs").is_some() {
            issues.push("project.dbdocs → move to top-level dbml".to_string());
        }
    }

    if doc.get("extensions").is_some() {
        issues.push("top-level extensions → move under target.<name>.extensions".to_string());
    }
    if doc.get("roles").is_some() {
        issues.push("top-level roles → move under target.<name>.roles".to_string());
    }
    if doc.get("supabase").is_some() {
        issues.push("top-level supabase → move to target.supabase.schemas".to_string());
    }
    if doc.get("convex").is_some() {
        issues.push("top-level convex → move to target.convex".to_string());
    }

    // Check for camelCase keys
    if let Some(import) = doc.get("import")
        && let Some(options) = import.get("options")
            && options.get("nullValue").is_some() {
                issues.push("import.options.nullValue → rename to null_value".to_string());
            }

    issues
}

/// Migrate a design.yaml from old (Node.js) format to the new Rust format.
///
/// Returns the migrated YAML content as a string.
pub fn migrate_config(content: &str) -> Result<String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(content)
        .map_err(|e| DbdError::Config(format!("Failed to parse YAML: {e}")))?;

    let mut new_doc = serde_yaml::Mapping::new();

    // ── project ────────────────────────────────────────
    let old_project = doc.get("project").and_then(|v| v.as_mapping());
    let mut project = serde_yaml::Mapping::new();

    if let Some(op) = old_project {
        if let Some(name) = op.get(val("name")) {
            project.insert(val("name"), name.clone());
        }
        if let Some(note) = op.get(val("note"))
            && !note.is_null() {
                project.insert(val("note"), note.clone());
            }
    }
    new_doc.insert(val("project"), serde_yaml::Value::Mapping(project));

    // ── source ─────────────────────────────────────────
    let dialect = old_project
        .and_then(|p| p.get(val("database")))
        .and_then(|v| v.as_str())
        .unwrap_or("PostgreSQL")
        .to_lowercase();

    let mut source = serde_yaml::Mapping::new();
    source.insert(val("dialect"), val(&dialect));
    new_doc.insert(val("source"), serde_yaml::Value::Mapping(source));

    // ── target ─────────────────────────────────────────
    let target_name = if doc.get("supabase").is_some() {
        "supabase"
    } else {
        match dialect.as_str() {
            "postgresql" | "postgres" => "postgres",
            "sqlite" => "sqlite",
            other => other,
        }
    };

    let mut target_config = serde_yaml::Mapping::new();

    // URL placeholder
    target_config.insert(val("url"), val("$DATABASE_URL"));

    // Extensions — apply extensionSchema to plain string entries
    let extension_schema = old_project
        .and_then(|p| p.get(val("extensionSchema")))
        .and_then(|v| v.as_str())
        .unwrap_or("public");

    if let Some(extensions) = doc.get("extensions").and_then(|v| v.as_sequence()) {
        let migrated_extensions: Vec<serde_yaml::Value> = extensions
            .iter()
            .map(|ext| {
                if ext.is_string() && extension_schema != "public" {
                    // Convert "uuid-ossp" → { name: "uuid-ossp", schema: "extensions" }
                    let mut map = serde_yaml::Mapping::new();
                    map.insert(val("name"), ext.clone());
                    map.insert(val("schema"), val(extension_schema));
                    serde_yaml::Value::Mapping(map)
                } else {
                    ext.clone()
                }
            })
            .collect();
        target_config.insert(
            val("extensions"),
            serde_yaml::Value::Sequence(migrated_extensions),
        );
    }

    // Roles
    if let Some(roles) = doc.get("roles") {
        target_config.insert(val("roles"), roles.clone());
    }

    // Supabase-specific: grants from schema entries + supabase schemas
    if target_name == "supabase" {
        if let Some(supabase) = doc.get("supabase") {
            target_config.insert(val("schemas"), supabase.clone());
        }

        // Extract grants from schema entries
        if let Some(schemas) = doc.get("schemas").and_then(|v| v.as_sequence()) {
            let mut grants = serde_yaml::Mapping::new();
            for entry in schemas {
                if let Some(map) = entry.as_mapping() {
                    for (key, value) in map {
                        if let Some(schema_config) = value.as_mapping()
                            && let Some(grant_config) = schema_config.get(val("grants")) {
                                grants.insert(key.clone(), grant_config.clone());
                            }
                    }
                }
            }
            if !grants.is_empty() {
                target_config.insert(val("grants"), serde_yaml::Value::Mapping(grants));
            }
        }
    }

    let mut targets = serde_yaml::Mapping::new();
    targets.insert(val(target_name), serde_yaml::Value::Mapping(target_config));
    new_doc.insert(val("target"), serde_yaml::Value::Mapping(targets));

    // ── schemas (strip grants, keep names only) ────────
    if let Some(schemas) = doc.get("schemas").and_then(|v| v.as_sequence()) {
        let clean_schemas: Vec<serde_yaml::Value> = schemas
            .iter()
            .map(|entry| {
                if let Some(map) = entry.as_mapping() {
                    // Extract just the schema name from { name: { grants: ... } }
                    if let Some((key, _)) = map.iter().next() {
                        return key.clone();
                    }
                }
                entry.clone()
            })
            .collect();
        new_doc.insert(
            val("schemas"),
            serde_yaml::Value::Sequence(clean_schemas),
        );
    }

    // ── external ───────────────────────────────────────
    if let Some(external) = doc.get("external") {
        new_doc.insert(val("external"), external.clone());
    }

    // ── import ─────────────────────────────────────────
    if let Some(import) = doc.get("import").and_then(|v| v.as_mapping()) {
        let mut new_import = serde_yaml::Mapping::new();

        // Move project.staging to import.staging
        if let Some(staging) = old_project.and_then(|p| p.get(val("staging"))) {
            new_import.insert(val("staging"), staging.clone());
        }

        // Migrate options (rename nullValue → null_value)
        if let Some(options) = import.get(val("options")).and_then(|v| v.as_mapping()) {
            let mut new_options = serde_yaml::Mapping::new();
            for (key, value) in options {
                let key_str = key.as_str().unwrap_or("");
                let new_key = match key_str {
                    "nullValue" => "null_value",
                    other => other,
                };
                new_options.insert(val(new_key), value.clone());
            }
            new_import.insert(val("options"), serde_yaml::Value::Mapping(new_options));
        }

        // tables, after — pass through (skip nulls, convert to empty sequences)
        if let Some(tables) = import.get(val("tables"))
            && !tables.is_null() {
                new_import.insert(val("tables"), tables.clone());
            }
        if let Some(after) = import.get(val("after")) {
            if after.is_null() {
                // Skip null values — the default empty vec handles this
            } else {
                new_import.insert(val("after"), after.clone());
            }
        }

        // Pass through any other import keys (env-specific after, schemas, etc.)
        for (key, value) in import {
            let key_str = key.as_str().unwrap_or("");
            if ["options", "tables", "after", "staging"].contains(&key_str) {
                continue; // Already handled
            }
            if !value.is_null() {
                new_import.insert(key.clone(), value.clone());
            }
        }

        new_doc.insert(val("import"), serde_yaml::Value::Mapping(new_import));
    }

    // ── export ─────────────────────────────────────────
    if let Some(export) = doc.get("export") {
        new_doc.insert(val("export"), export.clone());
    }

    // ── dbml (from project.dbdocs) ─────────────────────
    if let Some(dbdocs) = old_project.and_then(|p| p.get(val("dbdocs"))) {
        new_doc.insert(val("dbml"), dbdocs.clone());
    }

    // ── ignore (new, empty default) ────────────────────
    new_doc.insert(val("ignore"), serde_yaml::Value::Sequence(Vec::new()));

    let output = serde_yaml::to_string(&serde_yaml::Value::Mapping(new_doc))
        .map_err(|e| DbdError::Config(format!("Failed to serialize YAML: {e}")))?;

    Ok(output)
}

fn val(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.to_string())
}

// ── Plural DDL folder migration ────────────────────────────────────────────

/// Plural DDL type folders and their canonical singular form. dbd accepts both
/// when scanning, but singular is canonical; `dbd doctor --fix` migrates plural
/// folders to singular.
const PLURAL_DDL_DIRS: &[(&str, &str)] = &[
    ("tables", "table"),
    ("views", "view"),
    ("functions", "function"),
    ("procedures", "procedure"),
    ("enums", "enum"),
    ("roles", "role"),
];

/// A `ddl/<plural>/` folder that should use the canonical singular name.
#[derive(Debug, Clone)]
pub struct PluralDdlDir {
    /// e.g. `<project>/ddl/functions`
    pub plural: PathBuf,
    /// e.g. `<project>/ddl/function`
    pub singular: PathBuf,
}

/// Outcome of migrating one file (or folder) during plural → singular migration.
#[derive(Debug, Clone)]
pub enum DdlMoveOutcome {
    /// The whole folder was renamed (no singular folder existed yet).
    RenamedDir { from: PathBuf, to: PathBuf },
    /// A file was moved into the singular tree (no name collision).
    MovedFile { from: PathBuf, to: PathBuf },
    /// Same-path collision: the newer file (`winner`) was kept at `final_path`,
    /// the older file (`loser`) was backed up to `backup`.
    BackedUp {
        winner: PathBuf,
        loser: PathBuf,
        final_path: PathBuf,
        backup: PathBuf,
    },
    /// A filesystem error while handling a specific path.
    Error { path: PathBuf, error: String },
}

/// Detect `ddl/<plural>/` folders (e.g. `ddl/functions`) that should use the
/// canonical singular form (`ddl/function`).
pub fn detect_plural_ddl_dirs(project_dir: &Path) -> Vec<PluralDdlDir> {
    let ddl = project_dir.join("ddl");
    PLURAL_DDL_DIRS
        .iter()
        .filter_map(|(plural, singular)| {
            let plural_path = ddl.join(plural);
            plural_path.is_dir().then(|| PluralDdlDir {
                plural: plural_path,
                singular: ddl.join(singular),
            })
        })
        .collect()
}

/// Move a plural DDL folder to its singular form. If the singular folder does
/// not exist the whole folder is renamed; otherwise files are merged in,
/// resolving same-path collisions by keeping the newer file (by mtime) at the
/// singular path and backing the older one up to a `.bkp` sibling.
pub fn migrate_plural_ddl_dir(dir: &PluralDdlDir) -> Vec<DdlMoveOutcome> {
    if !dir.singular.exists() {
        return match std::fs::rename(&dir.plural, &dir.singular) {
            Ok(()) => vec![DdlMoveOutcome::RenamedDir {
                from: dir.plural.clone(),
                to: dir.singular.clone(),
            }],
            Err(e) => vec![DdlMoveOutcome::Error {
                path: dir.plural.clone(),
                error: e.to_string(),
            }],
        };
    }

    // Both folders exist — merge file by file.
    let mut outcomes = Vec::new();
    for src in collect_files(&dir.plural) {
        let rel = match src.strip_prefix(&dir.plural) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let dst = dir.singular.join(rel);
        outcomes.push(merge_one(&src, &dst));
    }

    // Drop now-empty plural directories (bottom-up). Anything left (e.g. a file
    // that errored) is preserved for the user to reconcile.
    remove_empty_dirs(&dir.plural);
    outcomes
}

/// Merge a single plural-folder file into its singular-tree destination.
fn merge_one(src: &Path, dst: &Path) -> DdlMoveOutcome {
    let err = |path: &Path, e: std::io::Error| DdlMoveOutcome::Error {
        path: path.to_path_buf(),
        error: e.to_string(),
    };

    if !dst.exists() {
        if let Some(parent) = dst.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return err(parent, e);
        }
        return match std::fs::rename(src, dst) {
            Ok(()) => DdlMoveOutcome::MovedFile {
                from: src.to_path_buf(),
                to: dst.to_path_buf(),
            },
            Err(e) => err(src, e),
        };
    }

    // Collision: the newest file (by mtime) is kept at `dst`; the older file is
    // backed up to `dst.bkp`. We always preserve the loser rather than delete it
    // — even an identical copy is kept as a backup.
    let backup = unique_backup_path(dst);
    let src_newer = match (mtime(src), mtime(dst)) {
        (Ok(s), Ok(d)) => s > d,
        // If a timestamp can't be read, prefer keeping the existing singular file.
        _ => false,
    };

    if src_newer {
        // Plural file wins: back up the existing singular file, then move src in.
        if let Err(e) = std::fs::rename(dst, &backup) {
            return err(dst, e);
        }
        match std::fs::rename(src, dst) {
            Ok(()) => DdlMoveOutcome::BackedUp {
                winner: src.to_path_buf(),
                loser: dst.to_path_buf(),
                final_path: dst.to_path_buf(),
                backup,
            },
            Err(e) => err(src, e),
        }
    } else {
        // Singular file wins (or tie): keep it, back up the plural file.
        match std::fs::rename(src, &backup) {
            Ok(()) => DdlMoveOutcome::BackedUp {
                winner: dst.to_path_buf(),
                loser: src.to_path_buf(),
                final_path: dst.to_path_buf(),
                backup,
            },
            Err(e) => err(src, e),
        }
    }
}

/// All files (recursively) under `dir`.
fn collect_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}

/// Remove empty directories under `root` (and `root` itself) bottom-up. Dirs
/// that still contain files are left in place.
fn remove_empty_dirs(root: &Path) {
    let mut dirs: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.into_path())
        .collect();
    // Deepest first so children are removed before parents.
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // ignores "not empty"
    }
}

/// `<dst>.bkp`, bumping a numeric suffix if that already exists.
fn unique_backup_path(dst: &Path) -> PathBuf {
    let mut base = dst.as_os_str().to_owned();
    base.push(".bkp");
    let first = PathBuf::from(&base);
    if !first.exists() {
        return first;
    }
    for n in 2.. {
        let mut candidate = base.clone();
        candidate.push(format!(".{n}"));
        let path = PathBuf::from(candidate);
        if !path.exists() {
            return path;
        }
    }
    unreachable!("backup path counter exhausted")
}

fn mtime(p: &Path) -> std::io::Result<std::time::SystemTime> {
    std::fs::metadata(p)?.modified()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_CONFIG: &str = r#"
project:
  name: Example
  database: PostgreSQL
  extensionSchema: extensions
  staging:
    - staging
  dbdocs:
    base:
      exclude:
        schemas:
          - staging

supabase:
  - config

schemas:
  - config:
      grants:
        anon: [usage, select]
  - staging

extensions:
  - uuid-ossp

roles:
  - name: basic

import:
  options:
    truncate: true
    nullValue: ''

export:
  - config.lookups
"#;

    #[test]
    fn detects_old_format_indicators() {
        let issues = detect_old_format(OLD_CONFIG);
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|i| i.contains("project.database")));
        assert!(issues.iter().any(|i| i.contains("project.staging")));
        assert!(issues.iter().any(|i| i.contains("extensions")));
        assert!(issues.iter().any(|i| i.contains("roles")));
        assert!(issues.iter().any(|i| i.contains("nullValue")));
        assert!(issues.iter().any(|i| i.contains("supabase")));
        assert!(issues.iter().any(|i| i.contains("dbdocs")));
    }

    #[test]
    fn detects_no_issues_in_new_format() {
        let new_config = r#"
project:
  name: Example

source:
  dialect: postgresql

target:
  postgres:
    url: $DATABASE_URL

schemas:
  - config
"#;
        let issues = detect_old_format(new_config);
        assert!(issues.is_empty());
    }

    #[test]
    fn migrates_project_name() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        assert!(result.contains("name: Example"));
    }

    #[test]
    fn migrates_source_dialect() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        assert!(result.contains("dialect: postgresql"));
    }

    #[test]
    fn migrates_target_with_extensions_and_roles() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        assert!(result.contains("supabase:"));
        assert!(result.contains("uuid-ossp"));
        assert!(result.contains("basic"));
    }

    #[test]
    fn migrates_schemas_without_grants() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        // Schema names should be plain strings now
        assert!(result.contains("- config"));
        assert!(result.contains("- staging"));
    }

    #[test]
    fn migrates_null_value_key() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        assert!(result.contains("null_value"));
        assert!(!result.contains("nullValue"));
    }

    #[test]
    fn migrates_staging_to_import() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        // staging should be under import, not project
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert!(parsed.get("import").unwrap().get("staging").is_some());
        assert!(parsed.get("project").unwrap().get("staging").is_none());
    }

    #[test]
    fn migrates_dbdocs_to_dbml() {
        let result = migrate_config(OLD_CONFIG).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        assert!(parsed.get("dbml").is_some());
        assert!(parsed.get("project").unwrap().get("dbdocs").is_none());
    }

    #[test]
    fn migrated_config_parses_with_new_structs() {
        let migrated = migrate_config(OLD_CONFIG).unwrap();
        let _config: crate::config::DesignConfig = serde_yaml::from_str(&migrated).unwrap();
    }

    // ── Plural DDL folder migration ────────────────────────

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn set_mtime(path: &Path, unix_secs: i64) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(unix_secs, 0)).unwrap();
    }

    #[test]
    fn detect_plural_ddl_dirs_finds_plural_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("ddl/functions/config")).unwrap();
        std::fs::create_dir_all(root.join("ddl/table/config")).unwrap(); // singular — ignored
        std::fs::create_dir_all(root.join("ddl/misc")).unwrap(); // not a type — ignored

        let found = detect_plural_ddl_dirs(root);
        assert_eq!(found.len(), 1);
        assert!(found[0].plural.ends_with("ddl/functions"));
        assert!(found[0].singular.ends_with("ddl/function"));
    }

    #[test]
    fn migrate_renames_when_singular_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("ddl/functions/config/f.ddl"), "CREATE FUNCTION");

        let dir = PluralDdlDir {
            plural: root.join("ddl/functions"),
            singular: root.join("ddl/function"),
        };
        let outcomes = migrate_plural_ddl_dir(&dir);

        assert!(matches!(outcomes.as_slice(), [DdlMoveOutcome::RenamedDir { .. }]));
        assert!(root.join("ddl/function/config/f.ddl").exists());
        assert!(!root.join("ddl/functions").exists());
    }

    #[test]
    fn migrate_merges_when_both_present_no_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("ddl/function/config/a.ddl"), "A");
        write_file(&root.join("ddl/functions/config/b.ddl"), "B");

        let dir = PluralDdlDir {
            plural: root.join("ddl/functions"),
            singular: root.join("ddl/function"),
        };
        let outcomes = migrate_plural_ddl_dir(&dir);

        assert!(outcomes.iter().any(|o| matches!(o, DdlMoveOutcome::MovedFile { .. })));
        assert!(root.join("ddl/function/config/a.ddl").exists()); // untouched
        assert!(root.join("ddl/function/config/b.ddl").exists()); // merged in
        assert!(!root.join("ddl/functions").exists());
    }

    #[test]
    fn migrate_collision_plural_newer_backs_up_singular() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sing = root.join("ddl/function/config/foo.ddl");
        let plur = root.join("ddl/functions/config/foo.ddl");
        write_file(&sing, "OLD-SINGULAR");
        write_file(&plur, "NEW-PLURAL");
        set_mtime(&sing, 1_000);
        set_mtime(&plur, 2_000); // plural is newer → it wins

        let dir = PluralDdlDir {
            plural: root.join("ddl/functions"),
            singular: root.join("ddl/function"),
        };
        let outcomes = migrate_plural_ddl_dir(&dir);

        assert_eq!(std::fs::read_to_string(&sing).unwrap(), "NEW-PLURAL");
        let bkp = root.join("ddl/function/config/foo.ddl.bkp");
        assert_eq!(std::fs::read_to_string(&bkp).unwrap(), "OLD-SINGULAR");
        assert!(outcomes.iter().any(|o| matches!(o, DdlMoveOutcome::BackedUp { .. })));
        assert!(!root.join("ddl/functions").exists());
    }

    #[test]
    fn migrate_collision_singular_newer_backs_up_plural() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sing = root.join("ddl/function/config/foo.ddl");
        let plur = root.join("ddl/functions/config/foo.ddl");
        write_file(&sing, "NEW-SINGULAR");
        write_file(&plur, "OLD-PLURAL");
        set_mtime(&sing, 2_000); // singular is newer → it wins
        set_mtime(&plur, 1_000);

        let dir = PluralDdlDir {
            plural: root.join("ddl/functions"),
            singular: root.join("ddl/function"),
        };
        migrate_plural_ddl_dir(&dir);

        assert_eq!(std::fs::read_to_string(&sing).unwrap(), "NEW-SINGULAR"); // kept
        let bkp = root.join("ddl/function/config/foo.ddl.bkp");
        assert_eq!(std::fs::read_to_string(&bkp).unwrap(), "OLD-PLURAL"); // loser backed up
        assert!(!root.join("ddl/functions").exists());
    }
}
