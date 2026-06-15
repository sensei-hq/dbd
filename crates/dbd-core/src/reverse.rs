//! Reverse-engineer engine: turn introspected entities into a write-plan over a
//! dbd project folder. Pure (no DB) except where it calls an adapter's
//! `introspect()`. See docs/superpowers/specs/2026-06-15-reverse-engineer-design.md.

/// Schemas always excluded (Postgres internals), regardless of flags.
pub const ALWAYS_EXCLUDED: &[&str] = &["pg_catalog", "information_schema"];

/// Supabase platform schemas excluded by default (overridable with `all=true`).
pub const SUPABASE_DENYLIST: &[&str] = &[
    "auth", "storage", "realtime", "_realtime", "extensions", "graphql",
    "graphql_public", "vault", "pgsodium", "pgsodium_masks", "supabase_functions",
    "supabase_migrations", "cron", "net", "pgbouncer", "_analytics", "_supavisor",
    "pgtle",
];

fn is_internal(schema: &str) -> bool {
    ALWAYS_EXCLUDED.contains(&schema)
        || schema.starts_with("pg_toast")
        || schema.starts_with("pg_temp")
}

/// Options controlling which schemas are reverse-engineered.
#[derive(Debug, Clone, Default)]
pub struct SchemaSelect {
    /// Allowlist — when non-empty, only these schemas are kept.
    pub only: Vec<String>,
    /// Extra schemas to exclude.
    pub exclude: Vec<String>,
    /// Bypass the Supabase denylist.
    pub all: bool,
}

/// Filter `db_schemas` (all schemas discovered in the DB) down to the set to emit.
pub fn select_schemas(db_schemas: &[String], opts: &SchemaSelect) -> Vec<String> {
    db_schemas
        .iter()
        .filter(|s| !is_internal(s))
        .filter(|s| opts.all || !SUPABASE_DENYLIST.contains(&s.as_str()))
        .filter(|s| !opts.exclude.iter().any(|e| e == *s))
        .filter(|s| opts.only.is_empty() || opts.only.iter().any(|o| o == *s))
        .cloned()
        .collect()
}

use crate::entity::{Entity, EntityType};
use std::path::PathBuf;

/// Map an entity to its DDL file path: `ddl/<kind>/<schema>/<name>.sql` for
/// schema-qualified kinds, `ddl/<kind>/<name>.sql` otherwise.
pub fn entity_path(entity: &Entity) -> PathBuf {
    let kind = entity.entity_type.tag(); // "table", "enum", "view", "schema", "extension"
    let name = entity.name.rsplit('.').next().unwrap_or(&entity.name);
    let mut p = PathBuf::from("ddl");
    p.push(&kind);
    if entity.entity_type.has_schema() && let Some(schema) = &entity.schema {
        p.push(schema);
    }
    p.push(format!("{name}.sql"));
    p
}

/// The entity kinds this command generates (used to scope orphan detection).
pub const MANAGED_KINDS: &[EntityType] = &[
    EntityType::Schema, EntityType::Extension, EntityType::Enum,
    EntityType::Table, EntityType::View,
];

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction { Create, Skip, Conflict }

#[derive(Debug, Clone)]
pub struct PlanItem {
    /// Path relative to the project root.
    pub path: PathBuf,
    pub content: String,
    pub action: FileAction,
}

#[derive(Debug, Default)]
pub struct WritePlan {
    pub items: Vec<PlanItem>,
    /// Existing managed-kind files under a selected schema with no generated counterpart.
    pub orphans: Vec<PathBuf>,
}

/// Classify each generated file vs disk, and detect orphans within `selected_schemas`
/// for the managed kinds only.
pub fn build_plan(
    root: &Path,
    generated: Vec<(PathBuf, String)>,
    selected_schemas: &[String],
) -> WritePlan {
    let mut items = Vec::new();
    let generated_paths: std::collections::HashSet<PathBuf> =
        generated.iter().map(|(p, _)| p.clone()).collect();

    for (rel, content) in generated {
        let abs = root.join(&rel);
        let action = match std::fs::read_to_string(&abs) {
            Ok(existing) if existing == content => FileAction::Skip,
            Ok(_) => FileAction::Conflict,
            Err(_) => FileAction::Create,
        };
        items.push(PlanItem { path: rel, content, action });
    }

    // Orphans: walk ddl/<managed-kind>/<selected-schema>/*.sql not in generated.
    let mut orphans = Vec::new();
    for kind in MANAGED_KINDS {
        if !kind.has_schema() {
            continue; // schema/extension live flat; skip orphan scan for them in v1
        }
        for schema in selected_schemas {
            let dir = root.join("ddl").join(kind.tag()).join(schema);
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                    continue;
                }
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                if !generated_paths.contains(&rel) {
                    orphans.push(rel);
                }
            }
        }
    }
    orphans.sort();
    WritePlan { items, orphans }
}

use crate::error::{DbdError, Result};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub created: usize,
    pub unchanged: usize,
    pub overwritten: usize,
    pub orphans: usize,
}

/// Pick a non-colliding `.bak` path: `a.sql.bak`, `a.sql.bak.1`, …
fn backup_path(file: &Path) -> PathBuf {
    let base = format!("{}.bak", file.display());
    let mut p = PathBuf::from(&base);
    let mut n = 1;
    while p.exists() {
        p = PathBuf::from(format!("{base}.{n}"));
        n += 1;
    }
    p
}

/// Apply a write-plan. Aborts (no writes) if there are conflicts and `!force`.
/// `dry_run` performs no writes. Orphans are counted, never touched.
pub fn apply_plan(root: &Path, plan: &WritePlan, force: bool, dry_run: bool) -> Result<Report> {
    let conflicts: Vec<&PlanItem> =
        plan.items.iter().filter(|i| i.action == FileAction::Conflict).collect();
    if !conflicts.is_empty() && !force {
        let list = conflicts.iter().map(|i| i.path.display().to_string())
            .collect::<Vec<_>>().join(", ");
        return Err(DbdError::Config(format!(
            "{} file conflict(s) — re-run with --force-overwrite to back up and replace: {list}",
            conflicts.len()
        )));
    }

    let mut report = Report { orphans: plan.orphans.len(), ..Default::default() };
    for it in &plan.items {
        match it.action {
            FileAction::Skip => report.unchanged += 1,
            FileAction::Create => {
                report.created += 1;
                if !dry_run {
                    let abs = root.join(&it.path);
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| DbdError::Config(format!("mkdir {}: {e}", parent.display())))?;
                    }
                    std::fs::write(&abs, &it.content)
                        .map_err(|e| DbdError::Config(format!("write {}: {e}", abs.display())))?;
                }
            }
            FileAction::Conflict => {
                report.overwritten += 1;
                if !dry_run {
                    let abs = root.join(&it.path);
                    let bak = backup_path(&abs);
                    std::fs::rename(&abs, &bak)
                        .map_err(|e| DbdError::Config(format!("backup {}: {e}", abs.display())))?;
                    std::fs::write(&abs, &it.content)
                        .map_err(|e| DbdError::Config(format!("write {}: {e}", abs.display())))?;
                }
            }
        }
    }
    Ok(report)
}

/// Emit DDL for each entity and build a write-plan against `root`.
/// Entities whose kind has no emitter (External, Function/Procedure) are skipped.
pub fn plan_from_entities(root: &Path, entities: &[Entity], selected_schemas: &[String]) -> WritePlan {
    let generated: Vec<(PathBuf, String)> = entities
        .iter()
        .filter(|e| MANAGED_KINDS.contains(&e.entity_type))
        .filter_map(|e| crate::emit::emit_entity(e).map(|sql| (entity_path(e), format!("{}\n", sql.trim_end()))))
        .collect();
    build_plan(root, generated, selected_schemas)
}

/// Render a `design.yaml` for a reverse-engineered project. The target URL is
/// always the env reference `$DATABASE_URL` — never the literal connection string.
pub fn design_yaml(project: &str, dialect: &str, schemas: &[String], version: u32) -> String {
    let target_key = if dialect == "sqlite" { "sqlite" } else { "postgres" };
    let schema_lines = schemas.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n");
    format!(
        "project:\n  name: {project}\n  version: {version}\n\n\
         source:\n  dialect: {dialect}\n\n\
         target:\n  {target_key}:\n    url: $DATABASE_URL\n\n\
         schemas:\n{schema_lines}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(xs: &[&str]) -> Vec<String> { xs.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn excludes_internal_and_supabase_by_default() {
        let db = v(&["public", "app", "pg_catalog", "auth", "storage", "pg_toast_x"]);
        let got = select_schemas(&db, &SchemaSelect::default());
        assert_eq!(got, v(&["public", "app"]));
    }

    #[test]
    fn all_schemas_keeps_supabase_but_not_pg_internal() {
        let db = v(&["public", "auth", "pg_catalog"]);
        let got = select_schemas(&db, &SchemaSelect { all: true, ..Default::default() });
        assert_eq!(got, v(&["public", "auth"]));
    }

    #[test]
    fn only_allowlist_wins() {
        let db = v(&["public", "app", "reporting"]);
        let got = select_schemas(&db, &SchemaSelect { only: v(&["app"]), ..Default::default() });
        assert_eq!(got, v(&["app"]));
    }

    #[test]
    fn exclude_adds_to_denies() {
        let db = v(&["public", "app"]);
        let got = select_schemas(&db, &SchemaSelect { exclude: v(&["app"]), ..Default::default() });
        assert_eq!(got, v(&["public"]));
    }

    #[test]
    fn entity_paths_follow_ddl_convention() {
        use std::path::PathBuf;
        let t = Entity::new(EntityType::Table, "shop.orders");
        assert_eq!(entity_path(&t), PathBuf::from("ddl/table/shop/orders.sql"));
        let e = Entity::new(EntityType::Enum, "shop.order_status");
        assert_eq!(entity_path(&e), PathBuf::from("ddl/enum/shop/order_status.sql"));
        let s = Entity::new(EntityType::Schema, "shop");
        assert_eq!(entity_path(&s), PathBuf::from("ddl/schema/shop.sql"));
    }

    #[test]
    fn build_plan_classifies_files() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // existing files: one identical, one differing, one orphan
        fs::create_dir_all(root.join("ddl/table/shop")).unwrap();
        fs::write(root.join("ddl/table/shop/orders.sql"), "SAME").unwrap();
        fs::write(root.join("ddl/table/shop/customers.sql"), "OLD").unwrap();
        fs::write(root.join("ddl/table/shop/legacy.sql"), "ORPHAN").unwrap();
        // an unmanaged kind that must NOT be flagged as orphan:
        fs::create_dir_all(root.join("ddl/function/shop")).unwrap();
        fs::write(root.join("ddl/function/shop/f.sql"), "FN").unwrap();

        let generated = vec![
            (PathBuf::from("ddl/table/shop/orders.sql"), "SAME".to_string()),     // skip
            (PathBuf::from("ddl/table/shop/customers.sql"), "NEW".to_string()),   // conflict
            (PathBuf::from("ddl/table/shop/products.sql"), "NEW".to_string()),    // create
        ];
        let plan = build_plan(root, generated, &["shop".to_string()]);

        let by = |a: FileAction| plan.items.iter().filter(|i| i.action == a)
            .map(|i| i.path.clone()).collect::<Vec<_>>();
        assert_eq!(by(FileAction::Skip), vec![PathBuf::from("ddl/table/shop/orders.sql")]);
        assert_eq!(by(FileAction::Conflict), vec![PathBuf::from("ddl/table/shop/customers.sql")]);
        assert_eq!(by(FileAction::Create), vec![PathBuf::from("ddl/table/shop/products.sql")]);
        assert_eq!(plan.orphans, vec![PathBuf::from("ddl/table/shop/legacy.sql")]);
        // function file is unmanaged → never an orphan
        assert!(!plan.orphans.iter().any(|p| p.to_string_lossy().contains("function")));
    }

    fn item(p: &str, c: &str, a: FileAction) -> PlanItem {
        PlanItem { path: PathBuf::from(p), content: c.into(), action: a }
    }

    #[test]
    fn apply_without_force_aborts_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let plan = WritePlan {
            items: vec![item("ddl/table/s/a.sql", "NEW", FileAction::Conflict)],
            orphans: vec![],
        };
        let err = apply_plan(dir.path(), &plan, /*force*/ false, /*dry_run*/ false).unwrap_err();
        assert!(err.to_string().contains("conflict"));
    }

    #[test]
    fn apply_with_force_backs_up_and_writes() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ddl/table/s")).unwrap();
        fs::write(dir.path().join("ddl/table/s/a.sql"), "OLD").unwrap();
        let plan = WritePlan {
            items: vec![
                item("ddl/table/s/a.sql", "NEW", FileAction::Conflict),
                item("ddl/table/s/b.sql", "B", FileAction::Create),
            ],
            orphans: vec![PathBuf::from("ddl/table/s/legacy.sql")],
        };
        let report = apply_plan(dir.path(), &plan, true, false).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.sql")).unwrap(), "NEW");
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.sql.bak")).unwrap(), "OLD");
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/b.sql")).unwrap(), "B");
        assert_eq!(report.created, 1);
        assert_eq!(report.overwritten, 1);
        assert_eq!(report.orphans, 1);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plan = WritePlan {
            items: vec![item("ddl/table/s/b.sql", "B", FileAction::Create)],
            orphans: vec![],
        };
        apply_plan(dir.path(), &plan, false, true).unwrap();
        assert!(!dir.path().join("ddl/table/s/b.sql").exists());
    }

    #[test]
    fn plan_from_entities_emits_and_classifies() {
        use crate::entity::{EnumValue};
        let dir = tempfile::tempdir().unwrap();
        let mut en = Entity::new(EntityType::Enum, "shop.status");
        en.enum_values = vec![EnumValue { name: "a".into(), note: None }];
        let entities = vec![Entity::new(EntityType::Schema, "shop"), en];

        let plan = plan_from_entities(dir.path(), &entities, &["shop".into()]);
        let paths: Vec<String> = plan.items.iter().map(|i| i.path.display().to_string()).collect();
        assert!(paths.contains(&"ddl/schema/shop.sql".to_string()));
        assert!(paths.contains(&"ddl/enum/shop/status.sql".to_string()));
        // content was emitted
        let enum_item = plan.items.iter().find(|i| i.path.ends_with("status.sql")).unwrap();
        assert!(enum_item.content.contains("CREATE TYPE"));
    }

    #[test]
    fn generates_design_yaml() {
        let yaml = design_yaml("shopdb", "postgresql", &["public".into(), "app".into()], 1);
        assert!(yaml.contains("name: shopdb"));
        assert!(yaml.contains("version: 1"));
        assert!(yaml.contains("dialect: postgresql"));
        assert!(yaml.contains("url: $DATABASE_URL")); // never the literal connection string
        assert!(yaml.contains("- public"));
        assert!(yaml.contains("- app"));
    }
}
