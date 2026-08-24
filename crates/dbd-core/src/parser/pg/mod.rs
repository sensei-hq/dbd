//! The Postgres-native DDL parser, built on libpg_query.
//!
//! Covers entity types incrementally. Anything not yet native delegates to
//! [`SqlparserDdl`], so the tree is releasable at every step of the migration
//! rather than only at the end.

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::Result;
use crate::parser::{DdlParser, SqlparserDdl};

/// libpg_query — PostgreSQL's own grammar.
pub(crate) struct PgQueryDdl;

impl PgQueryDdl {
    /// Entity types this parser handles itself.
    ///
    /// Single source of truth: dispatch below and `tests/parser_parity.rs` both
    /// read it, so a type cannot be switched over without also coming under the
    /// parity gate.
    pub(crate) const COVERED: &'static [EntityType] = &[];
}

impl DdlParser for PgQueryDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        // Nothing is native yet, so every type delegates. Types move into
        // COVERED one at a time, each behind the parity gate.
        SqlparserDdl.parse(file, sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SqlparserDdl;
    use std::path::Path;

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
    fn nothing_is_covered_yet() {
        assert!(PgQueryDdl::COVERED.is_empty());
    }
}
