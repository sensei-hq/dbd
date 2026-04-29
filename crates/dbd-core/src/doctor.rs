use std::path::Path;

use crate::error::{DbdError, Result};

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
    if let Some(import) = doc.get("import") {
        if let Some(options) = import.get("options") {
            if options.get("nullValue").is_some() {
                issues.push("import.options.nullValue → rename to null_value".to_string());
            }
        }
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
        if let Some(name) = op.get(&val("name")) {
            project.insert(val("name"), name.clone());
        }
        if let Some(note) = op.get(&val("note")) {
            if !note.is_null() {
                project.insert(val("note"), note.clone());
            }
        }
    }
    new_doc.insert(val("project"), serde_yaml::Value::Mapping(project));

    // ── source ─────────────────────────────────────────
    let dialect = old_project
        .and_then(|p| p.get(&val("database")))
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

    // Extensions
    if let Some(extensions) = doc.get("extensions") {
        target_config.insert(val("extensions"), extensions.clone());
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
                        if let Some(schema_config) = value.as_mapping() {
                            if let Some(grant_config) = schema_config.get(&val("grants")) {
                                grants.insert(key.clone(), grant_config.clone());
                            }
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
        if let Some(staging) = old_project.and_then(|p| p.get(&val("staging"))) {
            new_import.insert(val("staging"), staging.clone());
        }

        // Migrate options (rename nullValue → null_value)
        if let Some(options) = import.get(&val("options")).and_then(|v| v.as_mapping()) {
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
        if let Some(tables) = import.get(&val("tables")) {
            if !tables.is_null() {
                new_import.insert(val("tables"), tables.clone());
            }
        }
        if let Some(after) = import.get(&val("after")) {
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
    if let Some(dbdocs) = old_project.and_then(|p| p.get(&val("dbdocs"))) {
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
}
