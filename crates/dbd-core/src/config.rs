use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{DbdError, Result};

// ── Top-level config ────────────────────────────────────

/// Parsed design.yaml configuration.
#[derive(Debug, Deserialize)]
pub struct DesignConfig {
    pub project: ProjectConfig,
    #[serde(default)]
    pub source: SourceConfig,
    #[serde(default)]
    pub target: IndexMap<String, TargetConfig>,
    #[serde(default)]
    pub scopes: IndexMap<String, ScopeEntry>,
    #[serde(default)]
    pub schemas: Vec<SchemaEntry>,
    #[serde(default)]
    pub external: Vec<ExternalEntry>,
    #[serde(default)]
    pub import: ImportConfig,
    #[serde(default)]
    pub apply: ApplyConfig,
    #[serde(default)]
    pub export: Vec<ExportEntry>,
    #[serde(default)]
    pub materialized_views: MaterializedViewsConfig,
    #[serde(default)]
    pub dbml: HashMap<String, DbmlDocConfig>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub format: FormatConfig,
}

impl DesignConfig {
    /// The default target name (first listed in the config).
    pub fn default_target(&self) -> Option<&str> {
        self.target.keys().next().map(|s| s.as_str())
    }

    /// Get a target config by name, or the default if None.
    pub fn get_target(&self, name: Option<&str>) -> Option<&TargetConfig> {
        match name {
            Some(n) => self.target.get(n),
            None => self.target.values().next(),
        }
    }

    /// Schema names as a flat list (resolves both string and object forms).
    pub fn schema_names(&self) -> Vec<String> {
        self.schemas.iter().map(|s| s.name()).collect()
    }

    /// Schema→role→perms grants declared on the universal `schemas:` list via the
    /// `{ schema: { grants: { role: [perms] } } }` form. (Target-level `grants:` are
    /// merged in separately by the apply path.)
    pub fn schema_grants(&self) -> HashMap<String, HashMap<String, Vec<String>>> {
        self.schemas
            .iter()
            .filter_map(|s| match s {
                SchemaEntry::WithGrants(map) => {
                    let (schema, cfg) = map.iter().next()?;
                    Some((schema.clone(), cfg.grants.clone()?))
                }
                SchemaEntry::Name(_) => None,
            })
            .collect()
    }
}

// ── Scopes ──────────────────────────────────────────────

/// Dependency-gap policy for a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsPolicy {
    /// Gaps are errors; deploy refuses (default).
    #[default]
    Report,
    /// Deploy auto-expands to the dependency closure.
    Include,
}

/// A scope entry: either the bare string `all` (validated at resolve time —
/// any other bare string is a config error) or an include/exclude spec.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ScopeEntry {
    All(String),
    Spec(ScopeSpec),
}

/// Include/exclude selection plus dependency policy for one scope.
#[derive(Debug, Default, Deserialize)]
pub struct ScopeSpec {
    #[serde(default)]
    pub includes: Vec<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub deps: DepsPolicy,
    /// Per-scope extension allowlist. `None` = all target extensions apply
    /// (default). `Some(list)` = only these apply (`Some([])` = none) — lets a
    /// scope target a database that lacks an extension (e.g. an embedded PG
    /// without pgvector).
    ///
    /// List each extension by the same name dbd uses for it: the bare
    /// extension name (e.g. `vector`, `postgis`, `uuid-ossp`), NOT a
    /// schema-qualified name — even for extensions declared with a `schema:`,
    /// the entity name remains the bare extension name.
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
}

// ── Project ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub note: Option<String>,
    pub version: Option<u32>,
    /// Whether the project has been released (baselined). Once released, the
    /// declarative `dbd reconcile` workflow is disabled and schema changes must
    /// go through snapshots + migrations. Set by `dbd release`.
    #[serde(default)]
    pub released: bool,
}

// ── Source ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    #[serde(default = "default_dialect")]
    pub dialect: String,
    /// Which DDL parser reads this project's files. `None` lets `dialect`
    /// decide; set it only to override that choice.
    #[serde(default)]
    pub parser: Option<String>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            dialect: default_dialect(),
            parser: None,
        }
    }
}

fn default_dialect() -> String {
    "postgresql".to_string()
}

// ── Target ──────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct TargetConfig {
    pub url: Option<String>,
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub extensions: Vec<ExtensionEntry>,
    #[serde(default)]
    pub roles: Vec<RoleEntry>,
    pub schemas: Option<Vec<String>>,
    pub grants: Option<HashMap<String, GrantConfig>>,
    pub skip_schemas: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ExtensionEntry {
    Name(String),
    WithSchema { name: String, schema: String },
}

impl ExtensionEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::WithSchema { name, .. } => name,
        }
    }

    pub fn schema(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::WithSchema { schema, .. } => Some(schema),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RoleEntry {
    pub name: String,
    #[serde(default)]
    pub refers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GrantConfig {
    #[serde(flatten)]
    pub roles: HashMap<String, Vec<String>>,
}

// ── Schema ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SchemaEntry {
    Name(String),
    WithGrants(HashMap<String, SchemaGrantConfig>),
}

impl SchemaEntry {
    pub fn name(&self) -> String {
        match self {
            Self::Name(n) => n.clone(),
            Self::WithGrants(map) => map.keys().next().cloned().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SchemaGrantConfig {
    pub grants: Option<HashMap<String, Vec<String>>>,
}

// ── External ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExternalEntry {
    pub name: String,
    pub note: Option<String>,
    #[serde(default)]
    pub columns: Vec<HashMap<String, String>>,
}

// ── Import ──────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct ImportConfig {
    #[serde(default)]
    pub staging: Vec<String>,
    #[serde(default)]
    pub options: ImportOptions,
    #[serde(default)]
    pub tables: Vec<ImportTableEntry>,
    #[serde(default)]
    pub after: Vec<ScriptEntry>,
}

impl ImportConfig {
    /// Whether a staging table should be truncated before load. A per-table
    /// `tables:` override (`staging.x: { truncate: false }`) wins over the global
    /// `options.truncate`.
    pub fn table_truncate(&self, table_name: &str) -> bool {
        self.tables
            .iter()
            .find(|t| t.name() == table_name)
            .and_then(|t| match t {
                ImportTableEntry::WithOptions(map) => map.values().next().and_then(|o| o.truncate),
                ImportTableEntry::Name(_) => None,
            })
            .unwrap_or(self.options.truncate)
    }

    /// Per-table `format` override (`staging.x: { format: tsv }`), if set —
    /// forces that parser regardless of the data file's extension.
    pub fn table_format(&self, table_name: &str) -> Option<&str> {
        self.tables
            .iter()
            .find(|t| t.name() == table_name)
            .and_then(|t| match t {
                ImportTableEntry::WithOptions(map) => map.values().next().and_then(|o| o.format.as_deref()),
                ImportTableEntry::Name(_) => None,
            })
    }

    /// The null sentinel for a table's data load. A per-table `null_value`
    /// override (`staging.x: { null_value: "\\N" }`) wins over the global
    /// `options.null_value` (default `""` — an empty cell is NULL).
    pub fn table_null_value(&self, table_name: &str) -> &str {
        self.tables
            .iter()
            .find(|t| t.name() == table_name)
            .and_then(|t| match t {
                ImportTableEntry::WithOptions(map) => map.values().next().and_then(|o| o.null_value.as_deref()),
                ImportTableEntry::Name(_) => None,
            })
            .unwrap_or(&self.options.null_value)
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportOptions {
    #[serde(default = "default_true")]
    pub truncate: bool,
    #[serde(default)]
    pub null_value: String,
    #[serde(default = "default_csv")]
    pub format: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            truncate: default_true(),
            null_value: String::new(),
            format: default_csv(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_csv() -> String {
    "csv".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ImportTableEntry {
    Name(String),
    WithOptions(HashMap<String, ImportTableOptions>),
}

impl ImportTableEntry {
    pub fn name(&self) -> String {
        match self {
            Self::Name(n) => n.clone(),
            Self::WithOptions(map) => map.keys().next().cloned().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportTableOptions {
    pub truncate: Option<bool>,
    pub format: Option<String>,
    pub null_value: Option<String>,
    pub env: Option<EnvValue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    Single(String),
    Multiple(Vec<String>),
}

// ── Lifecycle script hooks ──────────────────────────────

/// A lifecycle hook script: a project-relative path, optionally with the tables
/// it touches declared explicitly.
///
/// Mirrors [`ImportTableEntry`]'s bare-or-object shape. The object form is not
/// an escape hatch for an exotic edge: measured against a real project's
/// realtime-publication hook, a script naming its tables inside a `format()`
/// string or an `array[…]` literal — never as a SQL identifier — is opaque to
/// every accessor libpg_query offers. Scope filtering has nothing to derive
/// unless the writes are told to it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ScriptEntry {
    Path(String),
    WithWrites { script: String, writes: Vec<String> },
}

impl ScriptEntry {
    pub fn script(&self) -> &str {
        match self {
            Self::Path(p) => p,
            Self::WithWrites { script, .. } => script,
        }
    }

    /// Explicitly declared tables, or `None` to derive them from the SQL.
    pub fn declared_writes(&self) -> Option<&[String]> {
        match self {
            Self::Path(_) => None,
            Self::WithWrites { writes, .. } => Some(writes),
        }
    }
}

/// Hooks around the DDL apply phase.
#[derive(Debug, Default, Deserialize)]
pub struct ApplyConfig {
    #[serde(default)]
    pub before: Vec<ScriptEntry>,
    #[serde(default)]
    pub after: Vec<ScriptEntry>,
}

// ── Export ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ExportEntry {
    Name(String),
    WithOptions(HashMap<String, ExportOptions>),
}

impl ExportEntry {
    pub fn name(&self) -> String {
        match self {
            Self::Name(n) => n.clone(),
            Self::WithOptions(map) => map.keys().next().cloned().unwrap_or_default(),
        }
    }

    /// Per-table format override, if set.
    pub fn format(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::WithOptions(map) => map.values().next()?.format.as_deref(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ExportOptions {
    pub format: Option<String>,
}

// ── Materialized views ───────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct MaterializedViewsConfig {
    #[serde(default)]
    pub options: MatviewOptions,
    #[serde(default)]
    pub overrides: HashMap<String, MatviewOverride>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatviewOptions {
    /// Shared cron schedule applied to every matview (pg_cron 5-field expression).
    #[serde(default)]
    pub refresh: Option<String>,
    /// Shared default for REFRESH ... CONCURRENTLY.
    #[serde(default)]
    pub concurrently: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatviewOverride {
    #[serde(default)]
    pub refresh: Option<String>,
    #[serde(default)]
    pub concurrently: Option<bool>,
}

/// Effective, resolved refresh settings for a single materialized view.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMatview {
    pub refresh: Option<String>,
    pub concurrently: bool,
}

impl MaterializedViewsConfig {
    /// Resolve effective settings for a matview by qualified name
    /// (`schema.name`): a per-view override overlays the global `options`.
    pub fn resolve(&self, name: &str) -> ResolvedMatview {
        let ov = self.overrides.get(name);
        ResolvedMatview {
            refresh: ov
                .and_then(|o| o.refresh.clone())
                .or_else(|| self.options.refresh.clone()),
            concurrently: ov.and_then(|o| o.concurrently).unwrap_or(self.options.concurrently),
        }
    }
}

// ── DBML ────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct DbmlDocConfig {
    pub include: Option<DbmlFilter>,
    pub exclude: Option<DbmlFilter>,
    /// Output filename for this document (relative to the directory the
    /// user passed to `dbd dbml`). Defaults to `<doc_key>.dbml`.
    pub output: Option<String>,
    /// When `true`, emit one `TableGroup <schema>` per schema present in
    /// the filtered table set.
    #[serde(default)]
    pub auto_group_by_schema: bool,
    /// Explicit table groups for this document.
    #[serde(default)]
    pub groups: Vec<DbmlGroupConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DbmlFilter {
    #[serde(default)]
    pub schemas: Vec<String>,
    #[serde(default)]
    pub tables: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct DbmlGroupConfig {
    pub name: String,
    #[serde(default)]
    pub tables: Vec<String>,
}

// ── Format ─────────────────────────────────────────────

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordCase {
    #[default]
    Lower,
    Upper,
    Preserve,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommaStyle {
    #[default]
    Leading,
    Trailing,
}

#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QueryStyle {
    /// No special query formatting — keyword case only.
    None,
    /// River-style formatting (default): right-aligned keywords, leading-comma
    /// SELECT lists, alias alignment, and AND/OR conditions aligned per clause.
    /// Queries the river renderer can't reproduce faithfully fall back to
    /// keyword-case-only automatically.
    #[default]
    River,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormatConfig {
    #[serde(default)]
    pub keyword_case: KeywordCase,
    #[serde(default)]
    pub comma_style: CommaStyle,
    #[serde(default = "default_type_alignment")]
    pub type_alignment: usize,
    #[serde(default = "default_indent")]
    pub indent: usize,
    /// Query formatting style for SELECT / VIEW bodies.
    #[serde(default)]
    pub query_style: QueryStyle,
    /// Width of the keyword gutter for river formatting (default: 10,
    /// which accommodates "inner join").
    #[serde(default = "default_gutter")]
    pub gutter: usize,
}

fn default_type_alignment() -> usize {
    27
}
fn default_indent() -> usize {
    2
}
fn default_gutter() -> usize {
    10
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            keyword_case: KeywordCase::Lower,
            comma_style: CommaStyle::Leading,
            type_alignment: default_type_alignment(),
            indent: default_indent(),
            query_style: QueryStyle::River,
            gutter: default_gutter(),
        }
    }
}

// ── Environment normalization ───────────────────────────

const ENV_ALIASES: &[(&str, &str)] = &[
    ("prod", "prod"),
    ("production", "prod"),
    ("dev", "dev"),
    ("development", "dev"),
];

/// Normalize environment string to "dev" or "prod".
pub fn normalize_env(value: Option<&str>) -> Result<String> {
    match value {
        None => Ok("prod".to_string()),
        Some(v) => ENV_ALIASES
            .iter()
            .find(|(alias, _)| *alias == v)
            .map(|(_, norm)| norm.to_string())
            .ok_or_else(|| {
                DbdError::Config(format!(
                    "Unknown environment: \"{v}\". Use dev, development, prod, or production."
                ))
            }),
    }
}

// ── File reading ────────────────────────────────────────

/// Read and parse a design.yaml file.
pub fn read(path: &Path) -> Result<DesignConfig> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let content = std::fs::read_to_string(path)
        .map_err(|e| DbdError::Config(format!("Cannot read {}: {}", path.display(), e)))?;
    let config: DesignConfig = serde_yaml::from_str(&content)?;
    Ok(config)
}

/// Update the `project.version` field in a design.yaml file.
pub fn update_version(config_path: &Path, version: u32) -> Result<()> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| DbdError::Config(format!("Cannot read {}: {}", config_path.display(), e)))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    value["project"]["version"] = serde_yaml::Value::Number(serde_yaml::Number::from(version));
    let output = serde_yaml::to_string(&value)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::write(config_path, output)?;
    Ok(())
}

/// Set the `project.released` flag in a design.yaml file.
pub fn set_released(config_path: &Path, released: bool) -> Result<()> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| DbdError::Config(format!("Cannot read {}: {}", config_path.display(), e)))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    value["project"]["released"] = serde_yaml::Value::Bool(released);
    let output = serde_yaml::to_string(&value)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::write(config_path, output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn normalize_env_defaults_to_prod() {
        assert_eq!(normalize_env(None).unwrap(), "prod");
    }

    #[test]
    fn normalize_env_accepts_aliases() {
        assert_eq!(normalize_env(Some("prod")).unwrap(), "prod");
        assert_eq!(normalize_env(Some("production")).unwrap(), "prod");
        assert_eq!(normalize_env(Some("dev")).unwrap(), "dev");
        assert_eq!(normalize_env(Some("development")).unwrap(), "dev");
    }

    #[test]
    fn normalize_env_rejects_unknown() {
        assert!(normalize_env(Some("staging")).is_err());
    }

    #[test]
    fn read_fixture_design_yaml() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.project.name, "example");
        assert_eq!(config.project.note, Some("Example project for testing".to_string()));
    }

    #[test]
    fn source_defaults_to_postgresql() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.source.dialect, "postgresql");
    }

    #[test]
    fn source_parser_is_optional_and_parses() {
        let cfg: DesignConfig = serde_yaml::from_str("project:\n  name: t\n").unwrap();
        assert_eq!(cfg.source.parser, None);
        let cfg: DesignConfig = serde_yaml::from_str("project:\n  name: t\nsource:\n  parser: pg_query\n").unwrap();
        assert_eq!(cfg.source.parser.as_deref(), Some("pg_query"));
        // SourceConfig carries both a manual Default impl and per-field serde
        // defaults; pin that they cannot drift apart for the new field.
        assert_eq!(cfg.source.dialect, "postgresql");
    }

    #[test]
    fn parses_target_config() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.default_target(), Some("postgres"));
        let target = config.get_target(None).unwrap();
        assert_eq!(target.url, Some("$DATABASE_URL".to_string()));
    }

    #[test]
    fn parses_extensions() {
        let config = read(&fixture("design.yaml")).unwrap();
        let target = config.get_target(Some("postgres")).unwrap();
        assert_eq!(target.extensions.len(), 3);
        assert_eq!(target.extensions[0].name(), "uuid-ossp");
        assert_eq!(target.extensions[1].name(), "postgis");
        assert_eq!(target.extensions[1].schema(), Some("extensions"));
        assert_eq!(target.extensions[2].name(), "pg_cron");
    }

    #[test]
    fn parses_roles() {
        let config = read(&fixture("design.yaml")).unwrap();
        let target = config.get_target(Some("postgres")).unwrap();
        assert_eq!(target.roles.len(), 2);
        assert_eq!(target.roles[0].name, "advanced");
        assert_eq!(target.roles[0].refers, vec!["basic"]);
        assert_eq!(target.roles[1].name, "basic");
        assert!(target.roles[1].refers.is_empty());
    }

    #[test]
    fn parses_schemas() {
        let config = read(&fixture("design.yaml")).unwrap();
        let names = config.schema_names();
        assert_eq!(names, vec!["config", "staging", "extensions"]);
    }

    #[test]
    fn parses_external_entities() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.external.len(), 1);
        assert_eq!(config.external[0].name, "auth.users");
        assert_eq!(
            config.external[0].note,
            Some("Managed authentication table".to_string())
        );
    }

    #[test]
    fn parses_import_config() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.import.staging, vec!["staging"]);
        assert!(config.import.options.truncate);
        assert_eq!(config.import.options.format, "csv");
        assert_eq!(config.import.tables.len(), 2);
        assert_eq!(config.import.tables[0].name(), "staging.lookups");
        assert_eq!(config.import.tables[1].name(), "staging.lookup_values");
        assert_eq!(config.import.after.len(), 1);
        assert_eq!(config.import.after[0].script(), "import/loader.sql");
    }

    #[test]
    fn import_table_overrides() {
        let yaml = r#"
project:
  name: test
import:
  options:
    truncate: true
  tables:
    - staging.keep
    - staging.no_truncate:
        truncate: false
    - staging.tsv:
        format: tsv
"#;
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        // truncate: a per-table `truncate: false` wins over the global `truncate: true`;
        // bare-name / unlisted tables inherit the global.
        assert!(!config.import.table_truncate("staging.no_truncate"));
        assert!(config.import.table_truncate("staging.keep"));
        assert!(config.import.table_truncate("staging.other"));
        // format: a per-table override is returned; otherwise None (fall back to the
        // file extension at load time).
        assert_eq!(config.import.table_format("staging.tsv"), Some("tsv"));
        assert_eq!(config.import.table_format("staging.keep"), None);
        assert_eq!(config.import.table_format("staging.other"), None);
    }

    #[test]
    fn import_table_null_value_overrides() {
        let yaml = r#"
project:
  name: test
import:
  tables:
    - staging.keep
    - staging.sentinel:
        null_value: "\\N"
"#;
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        // Global default is the empty string — an empty cell is NULL.
        assert_eq!(config.import.options.null_value, "");
        // Unlisted / bare-name tables inherit the global default.
        assert_eq!(config.import.table_null_value("staging.keep"), "");
        assert_eq!(config.import.table_null_value("staging.other"), "");
        // A per-table `null_value` override wins over the global.
        assert_eq!(config.import.table_null_value("staging.sentinel"), "\\N");
    }

    #[test]
    fn parses_export_config() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.export.len(), 2);
        assert_eq!(config.export[0].name(), "config.lookups");
        assert_eq!(config.export[1].name(), "config.lookup_values");
    }

    #[test]
    fn parses_dbml_config() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert!(config.dbml.contains_key("base"));
        let base = &config.dbml["base"];
        let exclude = base.exclude.as_ref().unwrap();
        assert_eq!(exclude.schemas, vec!["staging", "extensions"]);
    }

    #[test]
    fn parses_ignore_list() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert_eq!(config.ignore, vec!["bfs", "my_company.*"]);
    }

    #[test]
    fn get_target_returns_none_for_unknown() {
        let config = read(&fixture("design.yaml")).unwrap();
        assert!(config.get_target(Some("oracle")).is_none());
    }

    #[test]
    fn read_missing_file_returns_error() {
        let result = read(Path::new("nonexistent.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn parses_version_from_config() {
        let yaml = "project:\n  name: test\n  version: 5\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.project.version, Some(5));
    }

    #[test]
    fn parses_missing_version_as_none() {
        let yaml = "project:\n  name: test\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.project.version, None);
    }

    #[test]
    fn released_defaults_to_false_and_parses_true() {
        let config: DesignConfig = serde_yaml::from_str("project:\n  name: test\n").unwrap();
        assert!(!config.project.released, "released should default to false");
        let config: DesignConfig = serde_yaml::from_str("project:\n  name: test\n  released: true\n").unwrap();
        assert!(config.project.released);
    }

    #[test]
    fn set_released_writes_and_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("design.yaml");
        std::fs::write(&path, "project:\n  name: test\n  version: 1\n").unwrap();

        set_released(&path, true).unwrap();
        let config = read(&path).unwrap();
        assert!(config.project.released);
        // Existing fields survive the round-trip.
        assert_eq!(config.project.version, Some(1));

        set_released(&path, false).unwrap();
        assert!(!read(&path).unwrap().project.released);
    }

    // ── ExportEntry::format ───────────────────────────────

    #[test]
    fn export_entry_name_only_has_no_format() {
        let entry = ExportEntry::Name("app.users".to_string());
        assert_eq!(entry.name(), "app.users");
        assert!(entry.format().is_none());
    }

    #[test]
    fn export_entry_with_format_override() {
        let yaml = "app.logs:\n  format: jsonl\n";
        let entry: ExportEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.name(), "app.logs");
        assert_eq!(entry.format(), Some("jsonl"));
    }

    #[test]
    fn export_entry_with_options_no_format() {
        let yaml = "app.orders: {}\n";
        let entry: ExportEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.name(), "app.orders");
        assert!(entry.format().is_none());
    }

    #[test]
    fn update_version_writes_and_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("design.yaml");
        std::fs::write(&path, "project:\n  name: test\n  note: hello\n").unwrap();

        update_version(&path, 3).unwrap();

        let config = read(&path).unwrap();
        assert_eq!(config.project.version, Some(3));
        assert_eq!(config.project.name, "test");

        // Update again to verify overwriting works
        update_version(&path, 7).unwrap();
        let config = read(&path).unwrap();
        assert_eq!(config.project.version, Some(7));
    }

    #[test]
    fn parses_scope_object_form() {
        let yaml = "\
project:
  name: t
scopes:
  hub:
    includes: [config, app.users]
    deps: include
  reporting:
    excludes: [staging]
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let hub = match config.scopes.get("hub").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(hub.includes, vec!["config", "app.users"]);
        assert_eq!(hub.deps, DepsPolicy::Include);
        let rep = match config.scopes.get("reporting").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(rep.excludes, vec!["staging"]);
        assert_eq!(rep.deps, DepsPolicy::Report); // default
    }

    #[test]
    fn scope_extensions_allowlist_parses() {
        let yaml = "\
project:
  name: t
scopes:
  hive:
    includes: [config]
    extensions: [vector]
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let hive = match config.scopes.get("hive").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(hive.extensions, Some(vec!["vector".to_string()]));
    }

    #[test]
    fn scope_extensions_empty_list_parses_to_some_empty() {
        // `extensions: []` opts out of ALL extensions (distinct from absent).
        let yaml = "\
project:
  name: t
scopes:
  hive:
    includes: [config]
    extensions: []
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let hive = match config.scopes.get("hive").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(hive.extensions, Some(vec![]));
    }

    #[test]
    fn scope_without_extensions_field_parses_to_none() {
        // Absent field → None → today's always-on behavior preserved.
        let yaml = "\
project:
  name: t
scopes:
  hub:
    includes: [config]
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let hub = match config.scopes.get("hub").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(hub.extensions, None);
    }

    #[test]
    fn parses_scope_all_string_form() {
        let yaml = "project:\n  name: t\nscopes:\n  default: all\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config.scopes.get("default"), Some(ScopeEntry::All(s)) if s == "all"));
    }

    #[test]
    fn scopes_default_empty_when_absent() {
        let yaml = "project:\n  name: t\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.scopes.is_empty());
    }

    // ── Materialized views ────────────────────────────────

    #[test]
    fn resolves_matview_refresh_settings() {
        let yaml = r#"
project:
  name: t
materialized_views:
  options:
    refresh: "0 2 * * *"
    concurrently: true
  overrides:
    analytics.top_products:
      refresh: "*/30 * * * *"
    analytics.realtime:
      concurrently: false
"#;
        let cfg: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let mv = &cfg.materialized_views;

        let d = mv.resolve("analytics.daily_sales");
        assert_eq!(d.refresh.as_deref(), Some("0 2 * * *"));
        assert!(d.concurrently);

        let t = mv.resolve("analytics.top_products");
        assert_eq!(t.refresh.as_deref(), Some("*/30 * * * *"));
        assert!(t.concurrently); // inherited

        let r = mv.resolve("analytics.realtime");
        assert_eq!(r.refresh.as_deref(), Some("0 2 * * *")); // inherited
        assert!(!r.concurrently);
    }

    #[test]
    fn matview_without_global_schedule_has_no_refresh() {
        let yaml = "project:\n  name: t\n";
        let cfg: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let d = cfg.materialized_views.resolve("analytics.x");
        assert!(d.refresh.is_none());
        assert!(!d.concurrently);
    }

    // ── Schema grants ──────────────────────────────────────

    #[test]
    fn schema_grants_collects_with_grants_entries() {
        let yaml = "\
project:
  name: t
schemas:
  - config
  - staging:
      grants:
        app_user: [usage, select]
        app_admin: [usage, select, insert]
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let grants = config.schema_grants();
        // Plain `Name` entries (e.g. `config`) contribute nothing.
        assert_eq!(grants.len(), 1);
        let staging = grants.get("staging").unwrap();
        assert_eq!(
            staging.get("app_user"),
            Some(&vec!["usage".to_string(), "select".to_string()])
        );
        assert_eq!(
            staging.get("app_admin"),
            Some(&vec!["usage".to_string(), "select".to_string(), "insert".to_string()])
        );
    }

    #[test]
    fn schema_grants_empty_when_no_with_grants_entries() {
        let yaml = "project:\n  name: t\nschemas:\n  - config\n  - staging\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.schema_grants().is_empty());
    }

    #[test]
    fn schema_grants_skips_entry_with_no_grants_key() {
        // `{ schema: {} }` parses (grants: None) but contributes no map entry.
        let yaml = "project:\n  name: t\nschemas:\n  - staging: {}\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.schema_grants().is_empty());
    }

    #[test]
    fn a_bare_path_script_entry_parses() {
        let cfg: DesignConfig =
            serde_yaml::from_str("project:\n  name: t\nimport:\n  after:\n    - import/loader.sql\n").unwrap();
        assert_eq!(cfg.import.after.len(), 1);
        assert_eq!(cfg.import.after[0].script(), "import/loader.sql");
        assert!(cfg.import.after[0].declared_writes().is_none());
    }

    #[test]
    fn a_script_entry_with_explicit_writes_parses() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\nimport:\n  after:\n    - script: import/dyn.sql\n      writes: [app.target]\n",
        )
        .unwrap();
        assert_eq!(cfg.import.after[0].script(), "import/dyn.sql");
        assert_eq!(
            cfg.import.after[0].declared_writes(),
            Some(&vec!["app.target".to_string()][..])
        );
    }

    /// The two forms must mix in one list — a project migrating to explicit
    /// writes should not have to convert every entry at once.
    #[test]
    fn both_forms_mix_in_one_list() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\nimport:\n  after:\n    - a.sql\n    - script: b.sql\n      writes: [x.y]\n",
        )
        .unwrap();
        assert_eq!(cfg.import.after.len(), 2);
        assert!(cfg.import.after[0].declared_writes().is_none());
        assert!(cfg.import.after[1].declared_writes().is_some());
    }

    #[test]
    fn the_apply_block_parses_before_and_after() {
        let cfg: DesignConfig =
            serde_yaml::from_str("project:\n  name: t\napply:\n  before: [pre.sql]\n  after: [post.sql]\n").unwrap();
        assert_eq!(cfg.apply.before.len(), 1);
        assert_eq!(cfg.apply.after.len(), 1);
    }

    /// Every existing project omits `apply:` entirely.
    #[test]
    fn an_absent_apply_block_defaults_to_empty() {
        let cfg: DesignConfig = serde_yaml::from_str("project:\n  name: t\n").unwrap();
        assert!(cfg.apply.before.is_empty());
        assert!(cfg.apply.after.is_empty());
    }
}
