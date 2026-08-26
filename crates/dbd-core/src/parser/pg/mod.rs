//! The Postgres-native DDL parser, built on libpg_query.
//!
//! Covers entity types incrementally. Anything not yet native delegates to
//! [`SqlparserDdl`], so the tree is releasable at every step of the migration
//! rather than only at the end.

pub(crate) mod common;
pub(crate) mod enums;
pub(crate) mod matviews;
pub(crate) mod procs;
pub(crate) mod roles;
pub(crate) mod sequences;
pub(crate) mod tables;
pub(crate) mod views;

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::Result;
use crate::parser::{DdlParser, SqlparserDdl};

/// libpg_query — PostgreSQL's own grammar.
pub(crate) struct PgQueryDdl;

impl PgQueryDdl {
    /// Entity types this parser handles itself.
    ///
    /// Read by the parity harness (`tests/parser_parity.rs`) through
    /// [`crate::parser::pg_native_types`]. [`Self::native`] is the actual
    /// source of truth for dispatch; `covered_and_dispatch_cannot_drift` below
    /// pins the two together so this list can't claim a type `native` doesn't
    /// implement.
    pub(crate) const COVERED: &'static [EntityType] = &[
        EntityType::Enum,
        EntityType::View,
        EntityType::MaterializedView,
        EntityType::Function,
        EntityType::Procedure,
        EntityType::Role,
        EntityType::Table,
        EntityType::Sequence,
    ];

    /// The native parser for a type, or `None` when it still delegates.
    ///
    /// Single source of truth. `COVERED` is asserted against this, so a type
    /// cannot be listed as covered without an implementation — a mismatch used
    /// to fall through a wildcard match arm to sqlparser, leaving the parity
    /// gate comparing the incumbent against itself and passing for free.
    fn native(entity_type: EntityType) -> Option<fn(Entity, &str) -> Result<Entity>> {
        match entity_type {
            EntityType::Enum => Some(enums::parse_enum),
            EntityType::View => Some(views::parse_view),
            EntityType::MaterializedView => Some(matviews::parse_matview),
            EntityType::Function | EntityType::Procedure => Some(procs::parse_proc),
            EntityType::Role => Some(roles::parse_role),
            EntityType::Sequence => Some(sequences::parse_sequence),
            EntityType::Table => Some(tables::parse_table),
            _ => None,
        }
    }
}

impl DdlParser for PgQueryDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        let entity = Entity::from_file(file);
        match Self::native(entity.entity_type) {
            Some(parse) => parse(entity, sql),
            None => SqlparserDdl.parse(file, sql),
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
        let path = Path::new("ddl/sequence/app/s.ddl");
        let sql = "set search_path to app;\ncreate sequence s start with 5;";

        let old = SqlparserDdl.parse(path, sql).unwrap();
        let new = PgQueryDdl.parse(path, sql).unwrap();

        assert_eq!(json(&old), json(&new));
    }

    /// Every file-backed entity type is now native. `Schema` and `Extension`
    /// are synthesized from `design.yaml` rather than parsed from files — their
    /// folders are not even recognised DDL folders — so they are correctly
    /// absent, not overlooked.
    #[test]
    fn every_parsed_entity_type_is_native() {
        for t in [
            EntityType::Enum,
            EntityType::Table,
            EntityType::View,
            EntityType::MaterializedView,
            EntityType::Function,
            EntityType::Procedure,
            EntityType::Role,
            EntityType::Sequence,
        ] {
            assert!(PgQueryDdl::native(t).is_some(), "{t:?} is not native");
        }
    }

    /// COVERED and the dispatch match must agree for every entity type: a type
    /// listed but unimplemented would delegate silently and the parity gate
    /// would compare sqlparser against itself.
    #[test]
    fn covered_and_dispatch_cannot_drift() {
        use crate::entity::{TYPES_WITH_SCHEMA, TYPES_WITHOUT_SCHEMA};
        for t in TYPES_WITH_SCHEMA.iter().chain(TYPES_WITHOUT_SCHEMA) {
            assert_eq!(
                PgQueryDdl::COVERED.contains(t),
                PgQueryDdl::native(*t).is_some(),
                "COVERED and dispatch disagree on {t:?}"
            );
        }
    }
}
