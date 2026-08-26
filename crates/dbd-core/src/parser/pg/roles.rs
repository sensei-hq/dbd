//! Role DDL, parsed with libpg_query.
//!
//! Role files are idempotent-wrapped (`DO $$ … CREATE ROLE … END $$;`), which
//! sqlparser cannot read at all — hence the regex scanner this replaces. The
//! wrapper is a `DoStmt` here and simply ignored.
//!
//! The regex had to tell a role grant (`GRANT parent TO member`) from an object
//! grant (`GRANT SELECT ON TABLE t TO member`) by requiring `TO` to follow the
//! identifier immediately. Postgres parses them as different statement types, so
//! that exclusion is structural here: only `GrantRoleStmt` is a membership.

use crate::entity::{Entity, Reference};
use crate::error::Result;

/// Parse a role DDL file, recording its memberships as references.
///
/// Visible to `parser::mod` (not just `pg::mod`, unlike a purely native type):
/// `SqlparserDdl` calls this directly for `EntityType::Role` too, because
/// there has never been a sqlparser implementation of role DDL to fall back
/// to — see the call site's doc comment.
pub(in crate::parser) fn parse_role(mut entity: Entity, sql: &str) -> Result<Entity> {
    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    // Deliberately NOT setting `search_paths`: a role name carries no schema, so
    // nothing is qualified against it, and the sqlparser path never reached the
    // search-path extraction either. Populating it here would be drift.
    let mut names: Vec<String> = Vec::new();
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::GrantRoleStmt(grant)) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        for role in &grant.granted_roles {
            let Some(pg_query::NodeEnum::AccessPriv(priv_)) = role.node.as_ref() else {
                continue;
            };
            if priv_.priv_name.is_empty() || names.contains(&priv_.priv_name) {
                continue;
            }
            names.push(priv_.priv_name.clone());
        }
    }

    entity.references = names
        .iter()
        .map(|name| Reference {
            name: name.clone(),
            // A membership is a hard dependency, unlike a body's function calls.
            ref_type: None,
        })
        .collect();
    entity.refers = names;
    Ok(entity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        parse_role(Entity::new(EntityType::Role, "app_ro"), sql).unwrap()
    }

    /// The form `dbd` itself emits (`script::generate_role_script`), so this is
    /// the round-trip case.
    #[test]
    fn emitted_form_yields_its_memberships() {
        let e = parse(
            "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_ro') THEN\n    CREATE ROLE \"app_ro\";\n  END IF;\nEND $$;\nGRANT \"app_admin\" TO \"app_ro\";\n",
        );
        assert_eq!(e.refers, vec!["app_admin".to_string()]);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn bare_identifiers_are_handled() {
        let e = parse("grant parent to child;");
        assert_eq!(e.refers, vec!["parent".to_string()]);
    }

    /// An object grant is a different statement type in Postgres's grammar, so
    /// this exclusion is structural rather than a text-matching lookahead.
    #[test]
    fn object_grants_are_not_memberships() {
        let e =
            parse("GRANT SELECT ON TABLE t TO app_ro;\nGRANT INSERT, UPDATE ON ALL TABLES IN SCHEMA app TO app_ro;");
        assert!(e.refers.is_empty(), "object grants leaked: {:?}", e.refers);
    }

    #[test]
    fn multiple_memberships_are_all_captured() {
        let e = parse("grant a to c;\ngrant b to c;");
        assert_eq!(e.refers, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn duplicate_grants_are_deduplicated() {
        let e = parse("grant a to c;\ngrant a to c;");
        assert_eq!(e.refers, vec!["a".to_string()]);
    }

    #[test]
    fn with_admin_option_is_still_a_membership() {
        let e = parse("grant a to c with admin option;");
        assert_eq!(e.refers, vec!["a".to_string()]);
    }

    /// A membership is a hard reference — the resolver must not treat it as a
    /// soft one the way it treats a body's function calls.
    #[test]
    fn memberships_are_hard_references() {
        let e = parse("grant a to c;");
        assert_eq!(e.references[0].ref_type, None);
    }

    /// Role is the one native type whose search path stays empty: a role name
    /// has no schema, so nothing is qualified against it, and the incumbent
    /// never reached the search-path extraction. Setting it would fail parity.
    #[test]
    fn search_paths_stay_empty() {
        let e = parse("grant a to c;");
        assert!(e.search_paths.is_empty(), "got {:?}", e.search_paths);
    }

    /// Role names are not schema-qualified.
    #[test]
    fn role_names_are_not_schema_qualified() {
        let e = parse("grant a to c;");
        assert_eq!(e.refers, vec!["a".to_string()], "must not become public.a");
    }

    #[test]
    fn a_role_file_with_no_grants_has_no_refers() {
        let e = parse(
            "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'solo') THEN\n    CREATE ROLE \"solo\";\n  END IF;\nEND $$;\n",
        );
        assert!(e.refers.is_empty());
        assert!(
            e.errors.is_empty(),
            "a role with no grants is valid, got {:?}",
            e.errors
        );
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("grant to to to;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }
}
