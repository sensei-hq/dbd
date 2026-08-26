use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::error::{DbdError, Result};

/// Allowed DDL file extensions.
const DDL_EXTENSIONS: &[&str] = &["ddl", "sql"];

/// Allowed import data file extensions.
const IMPORT_EXTENSIONS: &[&str] = &["csv", "tsv", "json", "jsonl"];

/// Allowed policy file extensions.
const POLICY_EXTENSIONS: &[&str] = &["ddl", "sql"];

/// Scan a directory recursively and return all file paths matching the given extensions.
///
/// Fails loud: a `WalkDir` iterator error (permission denied, broken symlink, …)
/// aborts the scan instead of silently dropping the offending file/subtree.
fn scan_with_extensions(root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry.map_err(|e| DbdError::Config(format!("scan {}: {e}", root.display())))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let matches_ext = entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| extensions.contains(&ext));
        if matches_ext {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

/// Scan the `ddl/` folder for DDL files (.ddl, .sql).
pub fn scan_ddl(root: &Path) -> Result<Vec<PathBuf>> {
    let ddl_dir = root.join("ddl");
    let mut files = scan_with_extensions(&ddl_dir, DDL_EXTENSIONS)?;
    files.sort();
    Ok(files)
}

/// Outcome of scanning `import/`: the selected files plus everything that was
/// deliberately left out.
///
/// The exclusions are carried rather than discarded so callers can report why an
/// import loaded nothing. "No `import/` directory" and "every data file belongs
/// to a different env" both produce zero files, and a bare empty list cannot
/// tell those apart — which is exactly the case where a deploy looks like it
/// succeeded while loading no rows at all.
#[derive(Debug, Clone, Default)]
pub struct ImportScan {
    /// Data files selected for this env, sorted.
    pub files: Vec<PathBuf>,
    /// The project has no `import/` directory at all.
    pub dir_missing: bool,
    /// Data files excluded because the leading component under `import/` names a
    /// different env. Each entry is `(env_name, path_relative_to_import_dir)`.
    pub skipped_by_env: Vec<(String, PathBuf)>,
}

impl ImportScan {
    /// Distinct env names that owned at least one excluded file, sorted.
    pub fn skipped_envs(&self) -> Vec<&str> {
        let mut envs: Vec<&str> = self.skipped_by_env.iter().map(|(e, _)| e.as_str()).collect();
        envs.sort_unstable();
        envs.dedup();
        envs
    }
}

/// Scan the `import/` folder for data files (.csv, .tsv, .json, .jsonl).
///
/// When `env` is `None`, all files under `import/` are returned regardless of depth.
///
/// When `env` is `Some(name)`, the convention `import/{env}/{schema}/file` applies:
/// - Files at depth 1 under `import/` (`import/{schema}/file`) are always included.
/// - Files at depth 2+ under `import/` (`import/{first}/{…}/file`) are included only
///   when the first path component under `import/` matches `env`.
///
/// This lets projects keep shared seed data in `import/staging/` and
/// environment-specific fixtures in `import/dev/staging/` or `import/prod/staging/`.
///
/// Files excluded by the env rule are reported in [`ImportScan::skipped_by_env`]
/// rather than dropped silently.
pub fn scan_import(root: &Path, env: Option<&str>) -> Result<ImportScan> {
    let import_dir = root.join("import");
    if !import_dir.exists() {
        return Ok(ImportScan {
            dir_missing: true,
            ..Default::default()
        });
    }

    let mut scan = ImportScan::default();
    for entry in WalkDir::new(&import_dir) {
        let entry = entry.map_err(|e| DbdError::Config(format!("scan {}: {e}", import_dir.display())))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let matches_ext = entry
            .path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| IMPORT_EXTENSIONS.contains(&x));
        if !matches_ext {
            continue;
        }
        let relative = match entry.path().strip_prefix(&import_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Count directories between import/ and the file.
        let parent_depth = relative.parent().map(|p| p.components().count()).unwrap_or(0);
        // import/file and import/{schema}/file are always included; only
        // import/{env}/{…}/file is env-gated.
        let owning_env = if parent_depth <= 1 {
            None
        } else {
            relative.iter().next().and_then(|c| c.to_str()).map(|s| s.to_string())
        };

        match (owning_env, env) {
            // Depth-gated file whose env does not match the target env.
            (Some(owner), Some(target)) if owner != target => {
                scan.skipped_by_env.push((owner, relative.to_path_buf()));
            }
            _ => {
                let path = entry.path().to_path_buf();
                scan.files.push(path);
            }
        }
    }

    scan.files.sort();
    scan.skipped_by_env.sort();
    Ok(scan)
}

/// Scan the `policies/` folder for policy files (.ddl, .sql).
pub fn scan_policies(root: &Path) -> Result<Vec<PathBuf>> {
    let policies_dir = root.join("policies");
    let mut files = scan_with_extensions(&policies_dir, POLICY_EXTENSIONS)?;
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // DDL files
        fs::create_dir_all(root.join("ddl/table/config")).unwrap();
        fs::create_dir_all(root.join("ddl/view/config")).unwrap();
        fs::create_dir_all(root.join("ddl/procedure/staging")).unwrap();
        fs::create_dir_all(root.join("ddl/enum/config")).unwrap();
        fs::create_dir_all(root.join("ddl/role")).unwrap();
        fs::write(root.join("ddl/table/config/lookups.ddl"), "CREATE TABLE").unwrap();
        fs::write(root.join("ddl/table/config/lookup_values.ddl"), "CREATE TABLE").unwrap();
        fs::write(root.join("ddl/view/config/genders.ddl"), "CREATE VIEW").unwrap();
        fs::write(
            root.join("ddl/procedure/staging/import_lookups.ddl"),
            "CREATE PROCEDURE",
        )
        .unwrap();
        fs::write(root.join("ddl/enum/config/status.sql"), "CREATE TYPE").unwrap();
        fs::write(root.join("ddl/role/admin.ddl"), "CREATE ROLE").unwrap();

        // Non-DDL file (should be ignored)
        fs::write(root.join("ddl/table/config/README.md"), "docs").unwrap();

        // Import files
        fs::create_dir_all(root.join("import/staging")).unwrap();
        fs::create_dir_all(root.join("import/dev/staging")).unwrap();
        fs::write(root.join("import/staging/lookups.csv"), "id,name").unwrap();
        fs::write(root.join("import/staging/events.jsonl"), "{}").unwrap();
        fs::write(root.join("import/dev/staging/fixtures.csv"), "id").unwrap();

        // Non-import file (should be ignored)
        fs::write(root.join("import/staging/notes.txt"), "notes").unwrap();

        // Policy files
        fs::create_dir_all(root.join("policies/config")).unwrap();
        fs::write(root.join("policies/config/lookups.sql"), "CREATE POLICY").unwrap();

        tmp
    }

    #[test]
    fn scan_ddl_finds_all_ddl_and_sql_files() {
        let tmp = create_test_project();
        let files = scan_ddl(tmp.path()).unwrap();

        assert_eq!(files.len(), 6);
        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"lookups.ddl"));
        assert!(names.contains(&"lookup_values.ddl"));
        assert!(names.contains(&"genders.ddl"));
        assert!(names.contains(&"import_lookups.ddl"));
        assert!(names.contains(&"status.sql"));
        assert!(names.contains(&"admin.ddl"));
        // README.md should not be included
        assert!(!names.contains(&"README.md"));
    }

    #[test]
    fn scan_ddl_returns_empty_when_no_ddl_dir() {
        let tmp = TempDir::new().unwrap();
        let files = scan_ddl(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    /// File names of an `ImportScan`'s selected files, for terse assertions.
    fn selected_names(scan: &ImportScan) -> Vec<&str> {
        scan.files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect()
    }

    #[test]
    fn scan_import_finds_data_files() {
        let tmp = create_test_project();
        // No env filter: all data files returned
        let scan = scan_import(tmp.path(), None).unwrap();
        let files = &scan.files;

        assert_eq!(files.len(), 3);
        let names = selected_names(&scan);
        assert!(names.contains(&"lookups.csv"));
        assert!(names.contains(&"events.jsonl"));
        assert!(names.contains(&"fixtures.csv"));
        // notes.txt should not be included
        assert!(!names.contains(&"notes.txt"));
        // Nothing is env-gated when env is None, so nothing is reported as skipped.
        assert!(scan.skipped_by_env.is_empty());
        assert!(!scan.dir_missing);
    }

    /// A missing `import/` directory must be distinguishable from an `import/`
    /// directory that simply held no matching files — the caller reports the
    /// two differently, so an empty file list alone is not enough.
    #[test]
    fn scan_import_flags_missing_import_dir() {
        let tmp = TempDir::new().unwrap();
        let scan = scan_import(tmp.path(), None).unwrap();
        assert!(scan.files.is_empty());
        assert!(scan.dir_missing, "absent import/ must be flagged, not just empty");
    }

    /// An `import/` directory that exists but holds no data files is NOT
    /// "missing" — the project opted in and left it empty.
    #[test]
    fn scan_import_present_but_empty_dir_is_not_missing() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("import")).unwrap();
        let scan = scan_import(tmp.path(), None).unwrap();
        assert!(scan.files.is_empty());
        assert!(!scan.dir_missing, "an existing but empty import/ is not missing");
    }

    #[test]
    fn scan_import_env_includes_matching_env() {
        let tmp = create_test_project();
        // env="dev": depth-1 files + import/dev/** included; import/prod/** excluded
        let scan = scan_import(tmp.path(), Some("dev")).unwrap();
        let files = &scan.files;

        let names = selected_names(&scan);
        // Depth-1 files always included
        assert!(names.contains(&"lookups.csv"), "lookups.csv always included: {names:?}");
        assert!(
            names.contains(&"events.jsonl"),
            "events.jsonl always included: {names:?}"
        );
        // Dev-specific file included
        assert!(
            names.contains(&"fixtures.csv"),
            "fixtures.csv included for dev: {names:?}"
        );
        assert_eq!(files.len(), 3);
    }

    /// Excluding another env's fixtures must be *reported*, not silent — this is
    /// the case where a deploy under the wrong `--env` loads no rows and, before
    /// this, said nothing about the files it walked past.
    #[test]
    fn scan_import_reports_files_skipped_for_other_envs() {
        let tmp = create_test_project();
        let scan = scan_import(tmp.path(), Some("prod")).unwrap();

        assert_eq!(
            scan.skipped_envs(),
            vec!["dev"],
            "dev fixtures must be reported as skipped"
        );
        let skipped: Vec<&str> = scan
            .skipped_by_env
            .iter()
            .filter_map(|(_, p)| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert_eq!(skipped, vec!["fixtures.csv"]);
    }

    #[test]
    fn scan_import_env_excludes_other_envs() {
        let tmp = create_test_project();
        // env="prod": import/dev/** excluded, only depth-1 files returned
        let scan = scan_import(tmp.path(), Some("prod")).unwrap();
        let files = &scan.files;

        let names = selected_names(&scan);
        assert!(names.contains(&"lookups.csv"), "lookups.csv always included: {names:?}");
        assert!(
            names.contains(&"events.jsonl"),
            "events.jsonl always included: {names:?}"
        );
        assert!(
            !names.contains(&"fixtures.csv"),
            "fixtures.csv excluded for prod: {names:?}"
        );
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn scan_policies_finds_policy_files() {
        let tmp = create_test_project();
        let files = scan_policies(tmp.path()).unwrap();

        assert_eq!(files.len(), 1);
        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"lookups.sql"));
    }

    #[test]
    fn scan_policies_returns_empty_when_no_policies_dir() {
        let tmp = TempDir::new().unwrap();
        let files = scan_policies(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn scan_results_are_sorted() {
        let tmp = create_test_project();
        let files = scan_ddl(tmp.path()).unwrap();
        let sorted: Vec<PathBuf> = {
            let mut v = files.clone();
            v.sort();
            v
        };
        assert_eq!(files, sorted);
    }
}
