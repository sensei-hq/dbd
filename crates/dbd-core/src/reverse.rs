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
}
