use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Allowed DDL file extensions.
const DDL_EXTENSIONS: &[&str] = &["ddl", "sql"];

/// Allowed import data file extensions.
const IMPORT_EXTENSIONS: &[&str] = &["csv", "tsv", "json", "jsonl"];

/// Allowed policy file extensions.
const POLICY_EXTENSIONS: &[&str] = &["ddl", "sql"];

/// Scan a directory recursively and return all file paths matching the given extensions.
fn scan_with_extensions(root: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }

    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| extensions.contains(&ext))
        })
        .map(|entry| entry.into_path())
        .collect()
}

/// Scan the `ddl/` folder for DDL files (.ddl, .sql).
pub fn scan_ddl(root: &Path) -> Vec<PathBuf> {
    let ddl_dir = root.join("ddl");
    let mut files = scan_with_extensions(&ddl_dir, DDL_EXTENSIONS);
    files.sort();
    files
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
pub fn scan_import(root: &Path, env: Option<&str>) -> Vec<PathBuf> {
    let import_dir = root.join("import");
    if !import_dir.exists() {
        return Vec::new();
    }

    let mut files: Vec<PathBuf> = WalkDir::new(&import_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| IMPORT_EXTENSIONS.contains(&x))
        })
        .filter(|e| {
            let relative = match e.path().strip_prefix(&import_dir) {
                Ok(r) => r,
                Err(_) => return false,
            };
            // Count directories between import/ and the file.
            let parent_depth = relative.parent().map(|p| p.components().count()).unwrap_or(0);
            if parent_depth <= 1 {
                // import/file or import/{schema}/file — always included
                true
            } else {
                // import/{env}/{schema}/file — include only when env matches
                match env {
                    None => true,
                    Some(target) => relative
                        .iter()
                        .next()
                        .and_then(|c| c.to_str())
                        .is_some_and(|first| first == target),
                }
            }
        })
        .map(|e| e.into_path())
        .collect();

    files.sort();
    files
}

/// Scan the `policies/` folder for policy files (.ddl, .sql).
pub fn scan_policies(root: &Path) -> Vec<PathBuf> {
    let policies_dir = root.join("policies");
    let mut files = scan_with_extensions(&policies_dir, POLICY_EXTENSIONS);
    files.sort();
    files
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
        fs::write(root.join("ddl/procedure/staging/import_lookups.ddl"), "CREATE PROCEDURE").unwrap();
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
        let files = scan_ddl(tmp.path());

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
        let files = scan_ddl(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn scan_import_finds_data_files() {
        let tmp = create_test_project();
        // No env filter: all data files returned
        let files = scan_import(tmp.path(), None);

        assert_eq!(files.len(), 3);
        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"lookups.csv"));
        assert!(names.contains(&"events.jsonl"));
        assert!(names.contains(&"fixtures.csv"));
        // notes.txt should not be included
        assert!(!names.contains(&"notes.txt"));
    }

    #[test]
    fn scan_import_returns_empty_when_no_import_dir() {
        let tmp = TempDir::new().unwrap();
        let files = scan_import(tmp.path(), None);
        assert!(files.is_empty());
    }

    #[test]
    fn scan_import_env_includes_matching_env() {
        let tmp = create_test_project();
        // env="dev": depth-1 files + import/dev/** included; import/prod/** excluded
        let files = scan_import(tmp.path(), Some("dev"));

        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect();
        // Depth-1 files always included
        assert!(names.contains(&"lookups.csv"), "lookups.csv always included: {names:?}");
        assert!(names.contains(&"events.jsonl"), "events.jsonl always included: {names:?}");
        // Dev-specific file included
        assert!(names.contains(&"fixtures.csv"), "fixtures.csv included for dev: {names:?}");
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn scan_import_env_excludes_other_envs() {
        let tmp = create_test_project();
        // env="prod": import/dev/** excluded, only depth-1 files returned
        let files = scan_import(tmp.path(), Some("prod"));

        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"lookups.csv"), "lookups.csv always included: {names:?}");
        assert!(names.contains(&"events.jsonl"), "events.jsonl always included: {names:?}");
        assert!(!names.contains(&"fixtures.csv"), "fixtures.csv excluded for prod: {names:?}");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn scan_policies_finds_policy_files() {
        let tmp = create_test_project();
        let files = scan_policies(tmp.path());

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
        let files = scan_policies(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn scan_results_are_sorted() {
        let tmp = create_test_project();
        let files = scan_ddl(tmp.path());
        let sorted: Vec<PathBuf> = {
            let mut v = files.clone();
            v.sort();
            v
        };
        assert_eq!(files, sorted);
    }
}
