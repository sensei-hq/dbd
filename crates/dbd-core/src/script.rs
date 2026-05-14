
use crate::config::RoleEntry;
use crate::entity::{Entity, EntityType};

/// Supabase-managed schemas that must never be dropped.
pub const SUPABASE_PROTECTED: &[&str] = &[
    "auth", "storage", "realtime", "graphql_public",
    "supabase_functions", "pgbouncer", "pgsodium", "vault",
    "extensions", "supabase_migrations",
];

/// System schemas that must never be dropped.
pub const SYSTEM_PROTECTED: &[&str] = &[
    "pg_catalog", "information_schema", "pg_toast", "public",
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

/// Build a reset script that drops only declared user schemas.
///
/// Returns `Err` if any schema matches a protected list.
/// Filters out `skip_schemas` before generating DROP statements.
/// For postgres target: also drops roles in reverse dependency order.
pub fn build_reset_script(
    user_schemas: &[String],
    roles: &[RoleEntry],
    target: &str,
    skip_schemas: &[String],
) -> Result<Option<String>, String> {
    // Filter out skip_schemas first
    let candidate_schemas: Vec<&String> = user_schemas
        .iter()
        .filter(|s| !skip_schemas.iter().any(|skip| skip == *s))
        .collect();

    // Reject if any candidate matches protected lists
    for schema in &candidate_schemas {
        if target == "supabase" && SUPABASE_PROTECTED.contains(&schema.as_str()) {
            return Err(format!(
                "Cannot reset Supabase-protected schema: {}",
                schema
            ));
        }
        if SYSTEM_PROTECTED.contains(&schema.as_str()) {
            return Err(format!(
                "Cannot reset system-protected schema: {}",
                schema
            ));
        }
    }

    if candidate_schemas.is_empty() && roles.is_empty() {
        return Ok(None);
    }

    let mut lines = Vec::new();

    for schema in &candidate_schemas {
        lines.push(format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE;", schema));
    }

    if target == "postgres" {
        // Drop roles in reverse order (dependents first)
        for role in roles.iter().rev() {
            lines.push(format!("DROP ROLE IF EXISTS \"{}\";", role.name));
        }
    }

    Ok(Some(lines.join("\n")))
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

    #[test]
    fn reset_script_drops_schemas() {
        let schemas = vec!["config".to_string(), "staging".to_string()];
        let script = build_reset_script(&schemas, &[], "postgres", &[]).unwrap().unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"staging\" CASCADE"));
    }

    #[test]
    fn reset_script_postgres_drops_roles() {
        let schemas = vec!["config".to_string()];
        let roles = vec![
            RoleEntry { name: "basic".to_string(), refers: vec![] },
            RoleEntry { name: "advanced".to_string(), refers: vec!["basic".to_string()] },
        ];
        let script = build_reset_script(&schemas, &roles, "postgres", &[]).unwrap().unwrap();
        assert!(script.contains("DROP ROLE IF EXISTS \"advanced\""));
        assert!(script.contains("DROP ROLE IF EXISTS \"basic\""));
        // Reverse order: advanced before basic
        let adv_pos = script.find("advanced").unwrap();
        let basic_pos = script.find("\"basic\"").unwrap();
        assert!(adv_pos < basic_pos);
    }

    #[test]
    fn reset_script_returns_none_when_empty() {
        let script = build_reset_script(&[], &[], "postgres", &[]).unwrap();
        assert!(script.is_none());
    }

    // ── R1: only drops declared schemas ──────────────────
    #[test]
    fn r1_only_drops_declared_schemas() {
        let schemas = vec!["config".to_string(), "staging".to_string()];
        let script = build_reset_script(&schemas, &[], "supabase", &[]).unwrap().unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"staging\" CASCADE"));
        // Should not contain any undeclared schemas
        assert!(!script.contains("auth"));
        assert!(!script.contains("storage"));
    }

    // ── R2: supabase protected rejected ──────────────────
    #[test]
    fn r2_supabase_protected_rejected() {
        let schemas = vec!["config".to_string(), "auth".to_string()];
        let result = build_reset_script(&schemas, &[], "supabase", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Supabase-protected"));
    }

    // ── R3: system protected rejected ────────────────────
    #[test]
    fn r3_system_protected_rejected() {
        let schemas = vec!["config".to_string(), "pg_catalog".to_string()];
        let result = build_reset_script(&schemas, &[], "postgres", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("system-protected"));
    }

    // ── R4: skip_schemas excluded ────────────────────────
    #[test]
    fn r4_skip_schemas_excluded() {
        let schemas = vec!["config".to_string(), "staging".to_string()];
        let skip = vec!["staging".to_string()];
        let script = build_reset_script(&schemas, &[], "postgres", &skip).unwrap().unwrap();
        assert!(script.contains("config"));
        assert!(!script.contains("staging"));
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
