
use crate::config::RoleEntry;
use crate::entity::{Entity, EntityType};

/// Supabase-managed schemas that must never be dropped — includes `public`, which
/// Supabase exposes via PostgREST (on a plain `postgres` target `public` is an
/// ordinary project schema and is droppable with `--schemas`).
pub const SUPABASE_PROTECTED: &[&str] = &[
    "public", "auth", "storage", "realtime", "graphql_public",
    "supabase_functions", "pgbouncer", "pgsodium", "vault",
    "extensions", "supabase_migrations",
];

/// Generate DDL SQL from an entity.
///
/// For schema/extension/role: generates CREATE statements.
/// For file-based entities (table, view, etc.): reads the DDL file.
pub fn ddl_from_entity(entity: &Entity) -> Option<String> {
    match entity.entity_type {
        EntityType::Schema => Some(format!(
            "CREATE SCHEMA IF NOT EXISTS \"{}\";",
            entity.name
        )),
        EntityType::Extension => {
            let schema = entity.schema.as_deref().unwrap_or("public");
            Some(format!(
                "CREATE EXTENSION IF NOT EXISTS \"{}\" WITH SCHEMA \"{}\";",
                entity.name, schema
            ))
        }
        EntityType::Role => Some(generate_role_script(entity)),
        EntityType::External => None,
        _ => entity.file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()),
    }
}

/// Generate an idempotent role creation script.
///
/// Uses a DO block to check pg_catalog.pg_roles before creating.
/// Grants referenced roles (from entity.refers).
fn generate_role_script(entity: &Entity) -> String {
    let name = &entity.name;
    let mut script = format!(
        "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{name}') THEN\n    CREATE ROLE \"{name}\";\n  END IF;\nEND $$;\n"
    );
    for granted_role in &entity.refers {
        script.push_str(&format!("GRANT \"{granted_role}\" TO \"{name}\";\n"));
    }
    script
}

/// True system schemas that are never dropped, on any target.
const ALWAYS_PROTECTED: &[&str] = &["pg_catalog", "information_schema", "pg_toast"];

/// Schema of an entity, defaulting to `public` when unqualified.
fn entity_schema(entity: &Entity) -> &str {
    entity.schema.as_deref().unwrap_or("public")
}

/// Single-quote-escape a string for embedding in a SQL literal.
fn sql_quote_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Escape a SQL identifier for embedding inside double quotes (doubles any `"`).
/// Identifiers here come from the project's own validated design, but quoting
/// defensively keeps a stray `"` from breaking out of the `DROP … "x"` statement.
fn quote_ident(s: &str) -> String {
    s.replace('"', "\"\"")
}

/// Whether a schema is protected from `DROP SCHEMA` for the given target.
///
/// Always protects the true system schemas. On a `supabase` target, also
/// protects the full Supabase-managed set (including `public`). On `postgres`,
/// `public` is a normal project schema and is dropped.
fn schema_is_protected(schema: &str, target: &str) -> bool {
    if ALWAYS_PROTECTED.contains(&schema) {
        return true;
    }
    if target == "supabase" && SUPABASE_PROTECTED.contains(&schema) {
        return true;
    }
    false
}

/// Emit a `DROP ROUTINE` DO-block dropping every overload of `schema.name`
/// (functions and procedures alike, PG ≥ 11), scoped to that exact schema+name.
fn function_drop_block(schema: &str, name: &str) -> String {
    let s = sql_quote_escape(schema);
    let n = sql_quote_escape(name);
    format!(
        "DO $$ DECLARE r record; BEGIN\n  \
         FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p\n           \
         JOIN pg_namespace n ON n.oid = p.pronamespace\n           \
         WHERE n.nspname = '{s}' AND p.proname = '{n}'\n  \
         LOOP EXECUTE format('DROP ROUTINE IF EXISTS %s CASCADE', r.sig); END LOOP;\nEND $$;"
    )
}

/// Build an entity-level reset script.
///
/// Drops the project's own managed objects individually, in reverse dependency
/// order (functions/procedures → views → tables → sequences → enums). Tables,
/// views, enums and sequences use `DROP … IF EXISTS "schema"."name" CASCADE`
/// (schema defaults to `public`). Functions/procedures use a per-`schema.name`
/// `DO $$ … DROP ROUTINE … $$` block covering every overload.
///
/// `drop_schemas` additionally emits `DROP SCHEMA IF EXISTS "s" CASCADE;` for
/// each schema in `schemas`, silently skipping protected ones (always the true
/// system schemas; on a `supabase` target the full Supabase-managed set incl.
/// `public`). `drop_extensions` additionally emits `DROP EXTENSION IF EXISTS
/// "e" CASCADE;` for each name in `extensions`. Roles are dropped last (postgres
/// target only). Never errors on protected schemas — it skips them.
#[allow(clippy::too_many_arguments)]
pub fn build_reset_script(
    entities: &[&Entity],
    roles: &[RoleEntry],
    extensions: &[String],
    target: &str,
    drop_schemas: bool,
    drop_extensions: bool,
    schemas: &[String],
) -> Result<Option<String>, String> {
    let mut lines = entity_drop_lines(entities);

    // ── Optional schema DROPs (filtered by protected rules) ───
    if drop_schemas {
        for schema in schemas {
            if schema_is_protected(schema, target) {
                continue;
            }
            lines.push(format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE;", quote_ident(schema)));
        }
    }

    // ── Optional extension DROPs ──────────────────────────────
    if drop_extensions {
        for ext in extensions {
            lines.push(format!("DROP EXTENSION IF EXISTS \"{}\" CASCADE;", quote_ident(ext)));
        }
    }

    // ── Role DROPs (postgres only, reverse order) ─────────────
    if target == "postgres" {
        for role in roles.iter().rev() {
            lines.push(format!("DROP ROLE IF EXISTS \"{}\";", quote_ident(&role.name)));
        }
    }

    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(lines.join("\n")))
}

/// Entity DROPs in reverse dependency order:
/// functions/procedures → materialized views → views → tables → sequences →
/// enums. Materialized views come before plain views/tables because a matview
/// reads them. Function/procedure overloads share a name, so they collapse to
/// one DO-block per (schema, name).
fn entity_drop_lines(entities: &[&Entity]) -> Vec<String> {
    let mut funcs = Vec::new();
    let mut matviews = Vec::new();
    let mut views = Vec::new();
    let mut tables = Vec::new();
    let mut sequences = Vec::new();
    let mut enums = Vec::new();
    for e in entities {
        match e.entity_type {
            EntityType::Function | EntityType::Procedure => funcs.push(*e),
            EntityType::MaterializedView => matviews.push(*e),
            EntityType::View => views.push(*e),
            EntityType::Table => tables.push(*e),
            EntityType::Sequence => sequences.push(*e),
            EntityType::Enum => enums.push(*e),
            _ => {}
        }
    }

    let mut lines = Vec::new();
    let mut seen_funcs = std::collections::HashSet::new();
    for e in &funcs {
        let schema = entity_schema(e);
        let (_, bare) = crate::entity::split_qualified_name(&e.name);
        if seen_funcs.insert((schema.to_string(), bare.clone())) {
            lines.push(function_drop_block(schema, &bare));
        }
    }
    for e in &matviews {
        lines.push(drop_object("MATERIALIZED VIEW", e));
    }
    for e in &views {
        lines.push(drop_object("VIEW", e));
    }
    for e in &tables {
        lines.push(drop_object("TABLE", e));
    }
    for e in &sequences {
        lines.push(drop_object("SEQUENCE", e));
    }
    for e in &enums {
        lines.push(drop_object("TYPE", e));
    }
    lines
}

/// `DROP <KIND> IF EXISTS "schema"."name" CASCADE;` using the entity's bare name.
fn drop_object(kind: &str, entity: &Entity) -> String {
    let schema = quote_ident(entity_schema(entity));
    let (_, bare) = crate::entity::split_qualified_name(&entity.name);
    let bare = quote_ident(&bare);
    format!("DROP {kind} IF EXISTS \"{schema}\".\"{bare}\" CASCADE;")
}

/// Build a grants script for Supabase PostgREST schemas.
pub fn build_grants_script(
    schema_grants: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    supabase_schemas: &[String],
) -> Option<String> {
    if schema_grants.is_empty() && supabase_schemas.is_empty() {
        return None;
    }

    let mut lines = Vec::new();

    // Supabase schema grants
    for schema in supabase_schemas {
        for role in &["anon", "authenticated", "service_role"] {
            lines.push(format!("GRANT USAGE ON SCHEMA \"{}\" TO \"{}\";", schema, role));
        }
    }

    // Per-schema grants from config
    for (schema, role_perms) in schema_grants {
        for (role, perms) in role_perms {
            if perms.contains(&"usage".to_string()) {
                lines.push(format!("GRANT USAGE ON SCHEMA \"{}\" TO \"{}\";", schema, role));
            }
            let table_perms: Vec<&String> = perms
                .iter()
                .filter(|p| *p != "usage")
                .collect();
            if !table_perms.is_empty() {
                let perms_str = table_perms
                    .iter()
                    .map(|p| p.to_uppercase())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "GRANT {} ON ALL TABLES IN SCHEMA \"{}\" TO \"{}\";",
                    perms_str, schema, role
                ));
                lines.push(format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"{}\" GRANT {} ON TABLES TO \"{}\";",
                    schema, perms_str, role
                ));
            }
        }
    }

    if lines.is_empty() {
        return None;
    }

    // Notify PostgREST to reload
    lines.push("NOTIFY pgrst, 'reload config';".to_string());

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ddl_for_schema() {
        let entity = Entity::new(EntityType::Schema, "config");
        let ddl = ddl_from_entity(&entity).unwrap();
        assert_eq!(ddl, "CREATE SCHEMA IF NOT EXISTS \"config\";");
    }

    #[test]
    fn ddl_for_extension() {
        let mut entity = Entity::new(EntityType::Extension, "uuid-ossp");
        entity.schema = Some("extensions".to_string());
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("uuid-ossp"));
        assert!(ddl.contains("extensions"));
    }

    #[test]
    fn ddl_for_role_without_grants() {
        let entity = Entity::new(EntityType::Role, "basic");
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("CREATE ROLE"));
        assert!(ddl.contains("pg_roles"));
        assert!(!ddl.contains("GRANT"));
    }

    #[test]
    fn ddl_for_role_with_grants() {
        let mut entity = Entity::new(EntityType::Role, "advanced");
        entity.refers = vec!["basic".to_string()];
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("CREATE ROLE"));
        assert!(ddl.contains("GRANT \"basic\" TO \"advanced\""));
    }

    #[test]
    fn ddl_for_external_returns_none() {
        let entity = Entity::new(EntityType::External, "auth.users");
        assert!(ddl_from_entity(&entity).is_none());
    }

    // ── reset entity-set builders ─────────────────────────
    fn ent(ty: EntityType, name: &str) -> Entity {
        Entity::new(ty, name)
    }

    /// A representative project: public.users (table), config.lookups (table),
    /// a view, an enum, a sequence and a function.
    fn sample_entities() -> Vec<Entity> {
        vec![
            ent(EntityType::Table, "public.users"),
            ent(EntityType::Table, "config.lookups"),
            ent(EntityType::View, "config.genders"),
            ent(EntityType::Enum, "config.status"),
            ent(EntityType::Sequence, "config.counter"),
            ent(EntityType::Function, "config.touch"),
        ]
    }

    fn refs(entities: &[Entity]) -> Vec<&Entity> {
        entities.iter().collect()
    }

    #[test]
    fn reset_default_drops_entities_not_schemas() {
        let entities = sample_entities();
        let script = build_reset_script(&refs(&entities), &[], &[], "postgres", false, false, &[])
            .unwrap()
            .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(script.contains("DROP TABLE IF EXISTS \"config\".\"lookups\" CASCADE"));
        assert!(script.contains("DROP VIEW IF EXISTS \"config\".\"genders\" CASCADE"));
        assert!(script.contains("DROP TYPE IF EXISTS \"config\".\"status\" CASCADE"));
        assert!(script.contains("DROP SEQUENCE IF EXISTS \"config\".\"counter\" CASCADE"));
        // Function uses a DROP ROUTINE DO-block scoped to schema+name.
        assert!(script.contains("DO $$"));
        assert!(script.contains("DROP ROUTINE IF EXISTS"));
        assert!(script.contains("n.nspname = 'config' AND p.proname = 'touch'"));
        // No schema or extension drops on the default path; never errors on public.
        assert!(!script.contains("DROP SCHEMA"));
        assert!(!script.contains("DROP EXTENSION"));
    }

    #[test]
    fn reset_default_supabase_drops_public_entities_without_error() {
        // Regression: the old builder returned Err on `public`, aborting the
        // whole reset. On the default path (no schema drops) a supabase project
        // with public.users must drop the table and never error.
        let entities = sample_entities();
        let script = build_reset_script(&refs(&entities), &[], &[], "supabase", false, false, &[])
            .unwrap()
            .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA"));
    }

    #[test]
    fn reset_default_reverse_dependency_order() {
        let entities = sample_entities();
        let script = build_reset_script(&refs(&entities), &[], &[], "postgres", false, false, &[])
            .unwrap()
            .unwrap();
        let func = script.find("DROP ROUTINE").unwrap();
        let view = script.find("DROP VIEW").unwrap();
        let table = script.find("DROP TABLE").unwrap();
        let seq = script.find("DROP SEQUENCE").unwrap();
        let enm = script.find("DROP TYPE").unwrap();
        // functions/procedures → views → tables → sequences → enums
        assert!(func < view, "functions before views");
        assert!(view < table, "views before tables");
        assert!(table < seq, "tables before sequences");
        assert!(seq < enm, "sequences before enums");
    }

    #[test]
    fn reset_drops_materialized_view_before_its_deps() {
        let mut entities = sample_entities();
        // A matview reads config.genders (a view) — it must be dropped first.
        entities.push(ent(EntityType::MaterializedView, "config.genders_mv"));
        let script = build_reset_script(&refs(&entities), &[], &[], "postgres", false, false, &[])
            .unwrap()
            .unwrap();
        assert!(
            script.contains("DROP MATERIALIZED VIEW IF EXISTS \"config\".\"genders_mv\" CASCADE"),
            "reset must drop the materialized view; got:\n{script}"
        );
        // `find("DROP VIEW")` matches only the plain view — "DROP MATERIALIZED
        // VIEW" has no "DROP VIEW" substring.
        let mv = script.find("DROP MATERIALIZED VIEW").unwrap();
        let view = script.find("DROP VIEW").unwrap();
        let table = script.find("DROP TABLE").unwrap();
        assert!(mv < view, "materialized view dropped before plain views");
        assert!(mv < table, "materialized view dropped before tables");
    }

    #[test]
    fn reset_function_overloads_single_block() {
        // Two Function entities sharing schema.name collapse to one DO-block.
        let entities = vec![
            ent(EntityType::Function, "config.touch"),
            ent(EntityType::Function, "config.touch"),
        ];
        let script = build_reset_script(&refs(&entities), &[], &[], "postgres", false, false, &[])
            .unwrap()
            .unwrap();
        assert_eq!(script.matches("DO $$").count(), 1);
    }

    #[test]
    fn reset_drop_schemas_postgres_drops_public() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "public".to_string()];
        let script = build_reset_script(
            &refs(&entities), &[], &[], "postgres", true, false, &schemas,
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"public\" CASCADE"));
    }

    #[test]
    fn reset_drop_schemas_supabase_keeps_public_and_auth() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "public".to_string(), "auth".to_string()];
        let script = build_reset_script(
            &refs(&entities), &[], &[], "supabase", true, false, &schemas,
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"public\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"auth\" CASCADE"));
    }

    #[test]
    fn reset_drop_schemas_always_excludes_system() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "pg_catalog".to_string()];
        let script = build_reset_script(
            &refs(&entities), &[], &[], "postgres", true, false, &schemas,
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"pg_catalog\" CASCADE"));
    }

    #[test]
    fn reset_drop_extensions() {
        let entities = sample_entities();
        let extensions = vec!["vector".to_string(), "uuid-ossp".to_string()];
        let script = build_reset_script(
            &refs(&entities), &[], &extensions, "postgres", false, true, &[],
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP EXTENSION IF EXISTS \"vector\" CASCADE"));
        assert!(script.contains("DROP EXTENSION IF EXISTS \"uuid-ossp\" CASCADE"));
    }

    #[test]
    fn reset_clean_includes_schemas_and_extensions() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string()];
        let extensions = vec!["vector".to_string()];
        // --clean ⇒ both drop_schemas and drop_extensions on.
        let script = build_reset_script(
            &refs(&entities), &[], &extensions, "postgres", true, true, &schemas,
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP EXTENSION IF EXISTS \"vector\" CASCADE"));
    }

    #[test]
    fn reset_postgres_drops_roles_reverse_order() {
        let entities = sample_entities();
        let roles = vec![
            RoleEntry { name: "basic".to_string(), refers: vec![] },
            RoleEntry { name: "advanced".to_string(), refers: vec!["basic".to_string()] },
        ];
        let script = build_reset_script(
            &refs(&entities), &roles, &[], "postgres", false, false, &[],
        )
        .unwrap()
        .unwrap();
        let adv_pos = script.find("DROP ROLE IF EXISTS \"advanced\"").unwrap();
        let basic_pos = script.find("DROP ROLE IF EXISTS \"basic\"").unwrap();
        assert!(adv_pos < basic_pos);
    }

    #[test]
    fn reset_script_returns_none_when_empty() {
        let script = build_reset_script(&[], &[], &[], "postgres", false, false, &[]).unwrap();
        assert!(script.is_none());
    }

    #[test]
    fn grants_script_basic() {
        let mut schema_grants = HashMap::new();
        let mut config_grants = HashMap::new();
        config_grants.insert(
            "anon".to_string(),
            vec!["usage".to_string(), "select".to_string()],
        );
        schema_grants.insert("config".to_string(), config_grants);

        let script = build_grants_script(&schema_grants, &[]).unwrap();
        assert!(script.contains("GRANT USAGE ON SCHEMA \"config\" TO \"anon\""));
        assert!(script.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"config\" TO \"anon\""));
        assert!(script.contains("ALTER DEFAULT PRIVILEGES"));
        assert!(script.contains("NOTIFY pgrst"));
    }

    #[test]
    fn grants_script_returns_none_when_empty() {
        let script = build_grants_script(&HashMap::new(), &[]);
        assert!(script.is_none());
    }

}
