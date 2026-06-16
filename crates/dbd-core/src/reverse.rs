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

/// Returns `true` iff `s` is safe to use as a path segment on disk.
///
/// A segment is considered unsafe if it is:
/// - empty,
/// - the current-directory alias `.`,
/// - the parent-directory alias `..`,
/// - or contains any of `/`, `\`, or a NUL byte.
///
/// Entity names and schema names are written verbatim into file-system paths
/// (`ddl/<kind>/<schema>/<name>.ddl`).  Without this check a pathological but
/// technically-legal SQL identifier could escape the project root.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

use crate::entity::{Entity, EntityType};
use std::path::PathBuf;

/// Map an entity to its DDL file path: `ddl/<kind>/<schema>/<name>.ddl` for
/// schema-qualified kinds, `ddl/<kind>/<name>.ddl` otherwise.
///
/// `.ddl` is dbd's canonical extension — it is what `dbd init` scaffolds and what
/// existing projects use — so reverse-engineered files line up with (and `merge`
/// reconciles against) the files already on disk rather than creating `.sql`
/// duplicates. The scanner accepts both `.ddl` and `.sql`.
pub fn entity_path(entity: &Entity) -> PathBuf {
    let kind = entity.entity_type.tag(); // "table", "enum", "view", "schema", "extension"
    let name = entity.name.rsplit('.').next().unwrap_or(&entity.name);
    let mut p = PathBuf::from("ddl");
    p.push(&kind);
    if entity.entity_type.has_schema() && let Some(schema) = &entity.schema {
        p.push(schema);
    }
    p.push(format!("{name}.ddl"));
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
    /// Entities skipped because their schema or name failed the path-safety check.
    /// Each entry is `"<schema>.<name>"` (or just `"<name>"` for un-schema'd kinds).
    /// Callers should surface these as warnings so the user is aware.
    pub skipped_unsafe: Vec<String>,
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

    // Orphans: walk ddl/<managed-kind>/<selected-schema>/*.{ddl,sql} not in generated.
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
                // Accept both dbd DDL extensions so a stale file is flagged
                // regardless of which one the project uses.
                let is_ddl = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "ddl" || e == "sql");
                if !is_ddl {
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
    WritePlan { items, orphans, skipped_unsafe: vec![] }
}

use crate::error::{DbdError, Result};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub created: usize,
    pub unchanged: usize,
    pub overwritten: usize,
    pub orphans: usize,
    /// Number of items that conflict with existing on-disk content. In a real
    /// apply (`dry_run == false`) a non-empty count without `force` aborts before
    /// writing, so the report is only produced when conflicts are resolved (then
    /// they show up as `overwritten`). In a `dry_run` this counts the conflicts
    /// that *would* be backed up and overwritten.
    pub conflicts: usize,
}

/// Pick a non-colliding `.bak` path: `a.ddl.bak`, `a.ddl.bak.1`, …
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

/// Apply a write-plan to disk.
///
/// # Conflict gate (atomic)
/// If any item has [`FileAction::Conflict`] and `force` is `false`, the function returns
/// `Err` **before performing any write**.  This gate is atomic: either all writes proceed
/// or none do (subject to the caveat below).
///
/// # Non-transactionality (accepted v1 limitation)
/// Once past the conflict gate, items are written one by one.  If a write or rename fails
/// mid-loop (e.g. disk full, permission error), earlier items remain applied with their
/// `.bak` backups intact and the function returns `Err`.  There is no rollback.  Callers
/// should treat a mid-loop error as a partial apply and inspect the project directory.
///
/// `dry_run` performs no writes. Orphans are counted, never touched.
pub fn apply_plan(root: &Path, plan: &WritePlan, force: bool, dry_run: bool) -> Result<Report> {
    let conflicts: Vec<&PlanItem> =
        plan.items.iter().filter(|i| i.action == FileAction::Conflict).collect();
    // A real apply with conflicts and no --force is a hard abort: nothing is
    // written. A dry-run never errors here — it falls through to produce a plan
    // report (writing nothing), surfacing the conflict count instead.
    if !dry_run && !conflicts.is_empty() && !force {
        let list = conflicts.iter().map(|i| i.path.display().to_string())
            .collect::<Vec<_>>().join(", ");
        return Err(DbdError::Config(format!(
            "{} file conflict(s) — re-run with --force-overwrite to back up and replace: {list}",
            conflicts.len()
        )));
    }

    let mut report = Report {
        orphans: plan.orphans.len(),
        conflicts: conflicts.len(),
        ..Default::default()
    };
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

/// Apply a write-plan by **overwriting** every `Create` and `Conflict` item in
/// place — no `.bak`, no conflict gate.
///
/// This is the managed-database `merge` path (DB version ≥ project version): the
/// auto-snapshot taken afterwards plus version control are the safety net, so we
/// deliberately clobber on-disk drift rather than aborting or backing up. `Skip`
/// items (byte-identical to disk) are left untouched. Orphans are counted but
/// never deleted. Parent directories are created as needed.
///
/// # Non-transactionality (accepted limitation — same as `apply_plan`)
/// Items are written one by one. If a write fails mid-loop (e.g. disk full,
/// permission error), earlier files have already been overwritten with no `.bak`
/// and no rollback. The subsequent `create_snapshot` call in the caller can also
/// fail after files are written. In either case the partial-apply state should be
/// recovered via version control (`git checkout -- .` or equivalent).
pub fn apply_overwrite(root: &Path, plan: &WritePlan) -> Result<Report> {
    let mut report = Report {
        orphans: plan.orphans.len(),
        ..Default::default()
    };
    for it in &plan.items {
        match it.action {
            FileAction::Skip => report.unchanged += 1,
            FileAction::Create | FileAction::Conflict => {
                if it.action == FileAction::Create {
                    report.created += 1;
                } else {
                    report.overwritten += 1;
                }
                let abs = root.join(&it.path);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| DbdError::Config(format!("mkdir {}: {e}", parent.display())))?;
                }
                std::fs::write(&abs, &it.content)
                    .map_err(|e| DbdError::Config(format!("write {}: {e}", abs.display())))?;
            }
        }
    }
    report.conflicts = plan.items.iter().filter(|i| i.action == FileAction::Conflict).count();
    Ok(report)
}

/// Emit DDL for each entity and build a write-plan against `root`.
///
/// Entities whose kind has no emitter (External, Function/Procedure) are silently skipped.
/// Entities whose schema or bare name fails [`is_safe_segment`] are excluded from the plan
/// and reported in [`WritePlan::skipped_unsafe`] so the caller can surface a warning.
///
/// Each entity's emitted DDL is run through [`crate::formatter::format_ddl`] with `format`
/// so the generated bytes are **identical to what `dbd format` would produce**. This is what
/// makes the command idempotent: a file generated here and then passed through `dbd format`
/// is byte-for-byte unchanged, so a subsequent `dbd merge` classifies it as a skip rather
/// than a conflict. `format_ddl` falls back to leaving any statement it can't parse as-is,
/// so it never drops or mangles emitted constructs (schemas/extensions/enums/COMMENT ON).
pub fn plan_from_entities(
    root: &Path,
    entities: &[Entity],
    selected_schemas: &[String],
    format: &crate::config::FormatConfig,
) -> WritePlan {
    let mut skipped_unsafe: Vec<String> = Vec::new();
    let mut generated: Vec<(PathBuf, String)> = Vec::new();

    for e in entities {
        if !MANAGED_KINDS.contains(&e.entity_type) {
            continue;
        }
        // Derive the bare name (strip any schema prefix that was embedded in the name field).
        let bare_name = e.name.rsplit('.').next().unwrap_or(&e.name);

        // Check schema segment (when present).
        let schema_ok = e.schema.as_deref().is_none_or(is_safe_segment);
        // Check name segment.
        let name_ok = is_safe_segment(bare_name);

        if !schema_ok || !name_ok {
            let label = match &e.schema {
                Some(sc) => format!("{sc}.{}", e.name),
                None => e.name.clone(),
            };
            skipped_unsafe.push(label);
            continue;
        }

        if let Some(sql) = crate::emit::emit_entity(e) {
            // Run the emitted DDL through the formatter so the bytes match `dbd format`
            // exactly. `format_ddl` guarantees a single trailing newline, matching the
            // format command's file-write convention (it writes the formatter output
            // verbatim via `safe_write`).
            let content = crate::formatter::format_ddl(&sql, format);
            generated.push((entity_path(e), content));
        }
    }

    let mut plan = build_plan(root, generated, selected_schemas);
    plan.skipped_unsafe = skipped_unsafe;
    plan
}

/// Render a `design.yaml` for a reverse-engineered project.
///
/// The output matches the structure produced by `dbd init` (see [`crate::init`]):
/// - `project.name` + `project.version`
/// - `target.<dialect-key>.url` — always `$DATABASE_URL`, never a literal connection string
/// - `schemas` list populated from the discovered schema set
///
/// `dialect` should be `"postgres"`, `"supabase"`, or `"sqlite"`;
/// `"sqlite"` maps to the `sqlite` target key, everything else maps to `"postgres"`.
pub fn design_yaml(project: &str, dialect: &str, schemas: &[String], version: u32) -> String {
    let target_key = if dialect == "sqlite" { "sqlite" } else { "postgres" };
    let schema_lines = schemas.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n");
    format!(
        "project:\n  name: {project}\n  version: {version}\n\n\
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
        assert_eq!(entity_path(&t), PathBuf::from("ddl/table/shop/orders.ddl"));
        let e = Entity::new(EntityType::Enum, "shop.order_status");
        assert_eq!(entity_path(&e), PathBuf::from("ddl/enum/shop/order_status.ddl"));
        let s = Entity::new(EntityType::Schema, "shop");
        assert_eq!(entity_path(&s), PathBuf::from("ddl/schema/shop.ddl"));
    }

    #[test]
    fn build_plan_classifies_files() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // existing files: one identical, one differing, two orphans (one .ddl,
        // one .sql — orphan detection must catch both dbd extensions)
        fs::create_dir_all(root.join("ddl/table/shop")).unwrap();
        fs::write(root.join("ddl/table/shop/orders.ddl"), "SAME").unwrap();
        fs::write(root.join("ddl/table/shop/customers.ddl"), "OLD").unwrap();
        fs::write(root.join("ddl/table/shop/legacy.ddl"), "ORPHAN").unwrap();
        fs::write(root.join("ddl/table/shop/old.sql"), "ORPHAN").unwrap();
        // an unmanaged kind that must NOT be flagged as orphan:
        fs::create_dir_all(root.join("ddl/function/shop")).unwrap();
        fs::write(root.join("ddl/function/shop/f.ddl"), "FN").unwrap();

        let generated = vec![
            (PathBuf::from("ddl/table/shop/orders.ddl"), "SAME".to_string()),     // skip
            (PathBuf::from("ddl/table/shop/customers.ddl"), "NEW".to_string()),   // conflict
            (PathBuf::from("ddl/table/shop/products.ddl"), "NEW".to_string()),    // create
        ];
        let plan = build_plan(root, generated, &["shop".to_string()]);

        let by = |a: FileAction| plan.items.iter().filter(|i| i.action == a)
            .map(|i| i.path.clone()).collect::<Vec<_>>();
        assert_eq!(by(FileAction::Skip), vec![PathBuf::from("ddl/table/shop/orders.ddl")]);
        assert_eq!(by(FileAction::Conflict), vec![PathBuf::from("ddl/table/shop/customers.ddl")]);
        assert_eq!(by(FileAction::Create), vec![PathBuf::from("ddl/table/shop/products.ddl")]);
        assert_eq!(plan.orphans, vec![
            PathBuf::from("ddl/table/shop/legacy.ddl"),
            PathBuf::from("ddl/table/shop/old.sql"),
        ]);
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
            items: vec![item("ddl/table/s/a.ddl", "NEW", FileAction::Conflict)],
            orphans: vec![],
            skipped_unsafe: vec![],
        };
        let err = apply_plan(dir.path(), &plan, /*force*/ false, /*dry_run*/ false).unwrap_err();
        assert!(err.to_string().contains("conflict"));
    }

    #[test]
    fn dry_run_reports_conflicts_without_erroring_or_writing() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        // Seed an existing file so the conflict item has something to (not) back up.
        fs::create_dir_all(dir.path().join("ddl/table/s")).unwrap();
        fs::write(dir.path().join("ddl/table/s/a.ddl"), "OLD").unwrap();

        let plan = WritePlan {
            items: vec![
                item("ddl/table/s/a.ddl", "NEW", FileAction::Conflict),
                item("ddl/table/s/b.ddl", "B", FileAction::Create),
            ],
            orphans: vec![],
            skipped_unsafe: vec![],
        };

        // dry-run + conflict + no force must NOT error.
        let report = apply_plan(dir.path(), &plan, /*force*/ false, /*dry_run*/ true).unwrap();
        assert!(report.conflicts >= 1, "report should surface the conflict count");
        assert_eq!(report.conflicts, 1);

        // Nothing was written or backed up.
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.ddl")).unwrap(), "OLD");
        assert!(!dir.path().join("ddl/table/s/a.ddl.bak").exists());
        assert!(!dir.path().join("ddl/table/s/b.ddl").exists());
    }

    #[test]
    fn apply_with_force_backs_up_and_writes() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ddl/table/s")).unwrap();
        fs::write(dir.path().join("ddl/table/s/a.ddl"), "OLD").unwrap();
        let plan = WritePlan {
            items: vec![
                item("ddl/table/s/a.ddl", "NEW", FileAction::Conflict),
                item("ddl/table/s/b.ddl", "B", FileAction::Create),
            ],
            orphans: vec![PathBuf::from("ddl/table/s/legacy.ddl")],
            skipped_unsafe: vec![],
        };
        let report = apply_plan(dir.path(), &plan, true, false).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.ddl")).unwrap(), "NEW");
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.ddl.bak")).unwrap(), "OLD");
        assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/b.ddl")).unwrap(), "B");
        assert_eq!(report.created, 1);
        assert_eq!(report.overwritten, 1);
        assert_eq!(report.orphans, 1);
    }

    #[test]
    fn apply_overwrite_clobbers_without_bak() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Seed an existing file that the conflict item will overwrite, and an
        // identical file that must be skipped.
        fs::create_dir_all(root.join("ddl/table/s")).unwrap();
        fs::write(root.join("ddl/table/s/a.ddl"), "OLD").unwrap();
        fs::write(root.join("ddl/table/s/same.ddl"), "SAME").unwrap();

        let plan = WritePlan {
            items: vec![
                item("ddl/table/s/a.ddl", "NEW", FileAction::Conflict),
                item("ddl/table/s/b.ddl", "B", FileAction::Create),
                item("ddl/table/s/same.ddl", "SAME", FileAction::Skip),
            ],
            orphans: vec![PathBuf::from("ddl/table/s/legacy.ddl")],
            skipped_unsafe: vec![],
        };

        let report = apply_overwrite(root, &plan).unwrap();

        // Conflict overwritten in place — NO `.bak`.
        assert_eq!(fs::read_to_string(root.join("ddl/table/s/a.ddl")).unwrap(), "NEW");
        assert!(!root.join("ddl/table/s/a.ddl.bak").exists(), "apply_overwrite must not create .bak files");
        // Create written.
        assert_eq!(fs::read_to_string(root.join("ddl/table/s/b.ddl")).unwrap(), "B");
        // Skip left untouched.
        assert_eq!(fs::read_to_string(root.join("ddl/table/s/same.ddl")).unwrap(), "SAME");

        // Counts.
        assert_eq!(report.created, 1);
        assert_eq!(report.overwritten, 1);
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.conflicts, 1);
        assert_eq!(report.orphans, 1);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plan = WritePlan {
            items: vec![item("ddl/table/s/b.ddl", "B", FileAction::Create)],
            orphans: vec![],
            skipped_unsafe: vec![],
        };
        apply_plan(dir.path(), &plan, false, true).unwrap();
        assert!(!dir.path().join("ddl/table/s/b.ddl").exists());
    }

    #[test]
    fn plan_from_entities_emits_and_classifies() {
        use crate::entity::{EnumValue};
        let dir = tempfile::tempdir().unwrap();
        let mut en = Entity::new(EntityType::Enum, "shop.status");
        en.enum_values = vec![EnumValue { name: "a".into(), note: None }];
        let entities = vec![Entity::new(EntityType::Schema, "shop"), en];

        let plan = plan_from_entities(
            dir.path(),
            &entities,
            &["shop".into()],
            &crate::config::FormatConfig::default(),
        );
        let paths: Vec<String> = plan.items.iter().map(|i| i.path.display().to_string()).collect();
        assert!(paths.contains(&"ddl/schema/shop.ddl".to_string()));
        assert!(paths.contains(&"ddl/enum/shop/status.ddl".to_string()));
        // content was emitted (formatter lowercases keywords by default, so match
        // case-insensitively — the bytes are what `dbd format` would produce).
        let enum_item = plan.items.iter().find(|i| i.path.ends_with("status.ddl")).unwrap();
        assert!(enum_item.content.to_uppercase().contains("CREATE TYPE"));
    }

    /// End-to-end idempotency: a project generated by reverse and then run through
    /// `dbd format` must, on a *second* generate, classify every file as Skip — not
    /// Conflict. This is the regression guard for the `.bak`-churn bug: reverse now
    /// emits formatter-equivalent bytes, so the round trip is stable.
    #[test]
    fn generated_files_are_format_stable_and_skip_on_rerun() {
        use crate::entity::{
            ColumnDef, IndexColumn, IndexDef, IndexType, TableConstraint, TableDef,
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let fmt = crate::config::FormatConfig::default();

        // A representative table: columns, PK, a plain index and a GIN index, plus
        // a comment — exercises the table + index + COMMENT ON emit paths.
        let mut t = Entity::new(EntityType::Table, "shop.orders");
        t.schema = Some("shop".into());
        t.table_def = Some(TableDef {
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: false,
                    is_unique: false,
                    is_identity: false,
                    comment: None,
                    inline_fk: None,
                },
                ColumnDef {
                    name: "tags".into(),
                    data_type: "text[]".into(),
                    nullable: false,
                    default_value: Some("'{}'".into()),
                    is_pk: false,
                    is_unique: false,
                    is_identity: false,
                    comment: None,
                    inline_fk: None,
                },
            ],
            constraints: vec![TableConstraint::PrimaryKey {
                name: None,
                columns: vec!["id".into()],
            }],
            indexes: vec![
                IndexDef {
                    name: Some("orders_tags_idx".into()),
                    columns: vec![IndexColumn { name: "tags".into(), order: None }],
                    unique: false,
                    index_type: Some(IndexType::Gin),
                },
            ],
            comments: Default::default(),
        });
        let entities = vec![Entity::new(EntityType::Schema, "shop"), t];

        // Pass 1: generate into an empty project — everything is Create.
        let plan1 = plan_from_entities(root, &entities, &["shop".into()], &fmt);
        assert!(
            plan1.items.iter().all(|i| i.action == FileAction::Create),
            "first pass should be all Create: {:?}",
            plan1.items.iter().map(|i| (i.path.clone(), i.action)).collect::<Vec<_>>()
        );
        apply_plan(root, &plan1, false, false).unwrap();

        // Simulate `dbd format` over the project: read each generated file, format
        // it, and write it back. With the formatter-equivalent emit, this is a no-op.
        for it in &plan1.items {
            let abs = root.join(&it.path);
            let content = std::fs::read_to_string(&abs).unwrap();
            let formatted = crate::formatter::format_ddl(&content, &fmt);
            assert_eq!(
                content, formatted,
                "`dbd format` changed a reverse-generated file ({}): not idempotent",
                it.path.display()
            );
            std::fs::write(&abs, &formatted).unwrap();
        }

        // Pass 2: generate again over the (now formatted) project — every file must Skip.
        let plan2 = plan_from_entities(root, &entities, &["shop".into()], &fmt);
        for it in &plan2.items {
            assert_eq!(
                it.action,
                FileAction::Skip,
                "second pass must Skip (not Conflict) for {}",
                it.path.display()
            );
        }
        let report = apply_plan(root, &plan2, false, false).unwrap();
        assert_eq!(report.conflicts, 0, "second pass must have no conflicts");
        assert_eq!(report.unchanged, plan2.items.len());
    }

    #[test]
    fn generates_design_yaml() {
        let yaml = design_yaml("shopdb", "postgresql", &["public".into(), "app".into()], 1);
        // project block
        assert!(yaml.contains("name: shopdb"));
        assert!(yaml.contains("version: 1"));
        // target block — same structure as `dbd init` scaffold (no separate `source:` block)
        assert!(yaml.contains("target:"));
        assert!(yaml.contains("postgres:"));
        assert!(yaml.contains("url: $DATABASE_URL")); // never the literal connection string
        // schemas
        assert!(yaml.contains("- public"));
        assert!(yaml.contains("- app"));
        // must NOT contain the old `source: / dialect:` keys
        assert!(!yaml.contains("source:"));
        assert!(!yaml.contains("dialect:"));
    }

    #[test]
    fn design_yaml_sqlite_uses_sqlite_key() {
        let yaml = design_yaml("localdb", "sqlite", &["main".into()], 2);
        assert!(yaml.contains("sqlite:"));
        assert!(!yaml.contains("postgres:"));
        assert!(yaml.contains("url: $DATABASE_URL"));
        assert!(yaml.contains("version: 2"));
    }

    #[test]
    fn unsafe_path_segments_are_rejected() {
        use crate::entity::EnumValue;
        let dir = tempfile::tempdir().unwrap();

        // Entity with a path-traversal schema
        let mut evil_schema = Entity::new(EntityType::Enum, "status");
        evil_schema.schema = Some("../etc".to_string());
        evil_schema.enum_values = vec![EnumValue { name: "x".into(), note: None }];

        // Entity with a slash in the name
        let mut evil_name = Entity::new(EntityType::Table, "public/evil");
        evil_name.schema = Some("public".to_string());

        // A normal entity that must still be emitted
        let mut good = Entity::new(EntityType::Enum, "shop.status");
        good.schema = Some("shop".to_string());
        good.enum_values = vec![EnumValue { name: "a".into(), note: None }];

        let entities = vec![evil_schema, evil_name, good];
        let plan = plan_from_entities(
            dir.path(),
            &entities,
            &["shop".into()],
            &crate::config::FormatConfig::default(),
        );

        // The two unsafe entities must NOT appear in plan.items
        for item in &plan.items {
            let p = item.path.display().to_string();
            assert!(!p.contains(".."), "unsafe path escaped: {p}");
            assert!(!p.contains("evil"), "unsafe name in path: {p}");
        }

        // They must be reported in skipped_unsafe
        assert_eq!(plan.skipped_unsafe.len(), 2,
            "expected 2 skipped, got {:?}", plan.skipped_unsafe);

        // The good entity must be present
        let paths: Vec<String> = plan.items.iter().map(|i| i.path.display().to_string()).collect();
        assert!(paths.iter().any(|p| p.contains("status")),
            "good entity missing from plan: {:?}", paths);
    }
}
