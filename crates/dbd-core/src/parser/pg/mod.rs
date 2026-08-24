//! The Postgres-native DDL parser, built on libpg_query.
//!
//! Covers entity types incrementally. Anything not yet native delegates to
//! [`SqlparserDdl`], so the tree is releasable at every step of the migration
//! rather than only at the end.

pub(crate) mod enums;

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::Result;
use crate::parser::{DdlParser, SqlparserDdl};

/// libpg_query — PostgreSQL's own grammar.
pub(crate) struct PgQueryDdl;

impl PgQueryDdl {
    /// Entity types this parser handles itself.
    ///
    /// Single source of truth for the switchover. `PgQueryDdl::parse` branches
    /// on it as types go native, and the parity harness (`tests/parser_parity.rs`)
    /// reads the same list — so a type cannot switch over without also coming
    /// under the gate.
    pub(crate) const COVERED: &'static [EntityType] = &[EntityType::Enum];

    /// Whether this parser handles `entity_type` itself.
    pub(crate) fn covers(entity_type: EntityType) -> bool {
        Self::COVERED.contains(&entity_type)
    }
}

impl DdlParser for PgQueryDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        let entity = Entity::from_file(file);
        if !Self::covers(entity.entity_type) {
            return SqlparserDdl.parse(file, sql);
        }
        match entity.entity_type {
            EntityType::Enum => enums::parse_enum(entity, sql),
            // Unreachable while COVERED and this match agree; delegating rather
            // than panicking keeps a mismatch a non-event.
            _ => SqlparserDdl.parse(file, sql),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(entity: &Entity) -> serde_json::Value {
        serde_json::to_value(entity).expect("Entity serializes")
    }

    /// Until a type is in COVERED, PgQueryDdl must be byte-identical to the
    /// incumbent — that is what makes every step of the migration releasable.
    #[test]
    fn uncovered_types_delegate_identically() {
        let path = Path::new("ddl/table/app/t.ddl");
        let sql = "set search_path to app;\ncreate table t (id int primary key);";

        let old = SqlparserDdl.parse(path, sql).unwrap();
        let new = PgQueryDdl.parse(path, sql).unwrap();

        assert_eq!(json(&old), json(&new));
    }

    #[test]
    fn enum_is_covered() {
        assert!(PgQueryDdl::covers(EntityType::Enum));
        assert!(!PgQueryDdl::covers(EntityType::Table));
    }
}
