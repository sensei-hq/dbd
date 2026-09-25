//! The Postgres-native DDL parser, built on libpg_query.
//!
//! Covers every entity type `Entity::from_file` can produce. It was built
//! incrementally, delegating what it did not yet handle to a second sqlparser
//! implementation so the tree stayed releasable at each step; that second
//! implementation retired once the last type landed here.

pub(crate) mod common;
pub(in crate::parser) mod declarations;
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
use crate::parser::{DdlParser, ParsedFile};

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
            // Unreachable by construction, and left as a passthrough rather
            // than an error for that reason. `native` returns `None` only for
            // Schema, Extension, External and Import — none of which
            // `Entity::from_file` can produce: `EntityType::from_folder_name`
            // has no arm for them, and an unrecognised folder falls back to
            // Table (which is native). They are synthesized from design.yaml
            // and have no DDL body to read, so returning the entity as parsed
            // from its path is the correct answer if one ever arrives here.
            None => Ok(entity),
        }
    }
}

/// Read every entity a SQL file declares, with identity taken from the
/// statements rather than from a path.
///
/// Each declaration's own statements are sliced back out of `sql` and handed to
/// the same per-type parser [`PgQueryDdl::parse`] uses, so an entity from here
/// carries exactly what an entity from a DDL file carries — including the
/// reads/writes split on routines. The only difference is where the name,
/// schema and type came from.
pub(in crate::parser) fn parse_sql(sql: &str) -> Result<ParsedFile> {
    let search_paths = common::extract_search_paths_via_pg_query(sql);
    let default_schema = search_paths.first().cloned().unwrap_or_else(|| "public".to_string());

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ParsedFile {
                entities: Vec::new(),
                search_paths,
                errors: vec![format!("Parse error: {e}")],
            });
        }
    };

    let ambient = declarations::ambient_ranges(&parsed, sql);
    let mut entities = Vec::new();

    for decl in declarations::declarations(&parsed, sql, &default_schema) {
        let mut entity = Entity::new(decl.entity_type, &decl.name);
        entity.schema = decl.schema;

        // `native` returns `None` only for Schema and Extension, which declare
        // no structure to read — their identity is the whole entity.
        if let Some(parse) = PgQueryDdl::native(decl.entity_type) {
            let fragment = declarations::assemble(sql, &ambient, &decl.ranges);
            let name = entity.name.clone();
            let schema = entity.schema.clone();
            entity = parse(entity, &fragment)?;
            // The per-type parsers never touch identity — they were written for
            // a path-derived one. Restoring it here keeps that true by
            // construction rather than by trusting each of the eight.
            entity.name = name;
            entity.schema = schema;
        }
        entities.push(entity);
    }

    Ok(ParsedFile {
        entities,
        search_paths,
        errors: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
