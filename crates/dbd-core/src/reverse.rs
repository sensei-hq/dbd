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
}
