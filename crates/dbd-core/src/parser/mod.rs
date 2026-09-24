pub(crate) mod pg;

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};

/// Which parser reads a project's DDL.
///
/// One variant today. It was two during the libpg_query migration, when
/// `Sqlparser` held a second implementation as an escape hatch — but that
/// implementation hardcoded `PostgreSqlDialect`, so it was never a *dialect*
/// selector, only a second Postgres parser. It retired once every file-backed
/// type became native (see `pg::PgQueryDdl::COVERED`, or [`pg_native_types`]).
///
/// The type stays because [`Self::resolve`] is the seam a real dialect belongs
/// in: dbd reads PostgreSQL DDL only, and wiring a non-Postgres grammar means
/// adding a variant here rather than reviving the one that went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserChoice {
    /// libpg_query — PostgreSQL's own grammar, vendored from the server.
    PgQuery,
}

impl ParserChoice {
    /// `explicit` (`source.parser`) wins when set; otherwise the dialect decides.
    ///
    /// An unrecognised value is an error rather than a silent fallback: quietly
    /// ignoring a typo would leave the project on a parser its author did not
    /// choose, which is exactly the class of invisible behaviour the parser
    /// migration existed to remove. A *retired* value is held to the same bar,
    /// and named as retired so the message tells its author what happened.
    pub fn resolve(dialect: &str, explicit: Option<&str>) -> Result<Self> {
        match explicit {
            Some("pg_query") => Ok(Self::PgQuery),
            Some("sqlparser") => Err(DbdError::Config(
                "source.parser \"sqlparser\" was removed — it was a second PostgreSQL parser, \
                 not a dialect, and every entity type is now read by libpg_query. \
                 Drop the line, or set \"pg_query\"."
                    .to_string(),
            )),
            Some(other) => Err(DbdError::Config(format!(
                "unknown source.parser {other:?} — expected \"pg_query\""
            ))),
            None => Ok(Self::for_dialect(dialect)),
        }
    }

    /// The parser a `source.dialect` selects when `source.parser` is unset.
    ///
    /// Every dialect resolves to `PgQuery`, because PostgreSQL DDL is the only
    /// grammar dbd parses. That is not a regression for non-Postgres projects:
    /// `reverse::design_yaml` writes no `source:` block at all, so a project
    /// built by `dbd init --from-db sqlite://` has always loaded under the
    /// `postgresql` default and reached this parser anyway.
    ///
    /// SQLite DDL is *not* a Postgres subset — `AUTOINCREMENT`, `WITHOUT ROWID`
    /// and `STRICT` are rejected outright — so a SQLite project's own DDL does
    /// not round-trip today. Fixing that means teaching this function a real
    /// SQLite grammar; the parameter is unused until then, and kept so that
    /// work is a change of body rather than a change of signature.
    fn for_dialect(_dialect: &str) -> Self {
        Self::PgQuery
    }
}

/// Reads a DDL file into an [`Entity`].
///
/// One implementation. The trait is what a second one would be added against —
/// a real non-Postgres grammar, not the sqlparser twin that used to sit here.
pub(crate) trait DdlParser {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity>;
}

/// Parse a DDL file with an explicit parser choice.
///
/// The project scan (`design::from_config_with_dir`) resolves `source.parser`
/// once, before reading any file, and calls this directly so a bad config
/// value fails at load rather than partway through the scan.
pub fn parse_entity_with(choice: ParserChoice, file: &Path, sql: &str) -> Result<Entity> {
    match choice {
        ParserChoice::PgQuery => pg::PgQueryDdl.parse(file, sql),
    }
}

/// Parse a DDL file with the Postgres default parser.
///
/// Used by this crate's tests and by external embedders (see
/// `docs/design/architecture.md`); the project scan goes through
/// [`parse_entity_with`] with the choice resolved from `source.parser`.
pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
    parse_entity_with(ParserChoice::PgQuery, file, sql)
}

/// Entity types the Postgres-native parser handles itself.
///
/// Every file-backed type, which is what let the sqlparser implementation
/// retire. Kept public so a caller outside the crate can ask without reaching
/// into `pg::PgQueryDdl::COVERED`.
pub fn pg_native_types() -> &'static [EntityType] {
    pg::PgQueryDdl::COVERED
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ddl")
            .join(name)
    }

    fn parse_fixture(path: &str) -> Entity {
        let file = fixture(path);
        let sql = std::fs::read_to_string(&file).unwrap();
        parse_entity(&file, &sql).unwrap()
    }

    #[test]
    fn parses_table_entity_type() {
        let entity = parse_fixture("table/config/lookups.ddl");
        assert_eq!(entity.entity_type, EntityType::Table);
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn extracts_search_paths() {
        let entity = parse_fixture("table/config/lookups.ddl");
        assert_eq!(entity.search_paths, vec!["config", "extensions"]);
    }

    #[test]
    fn extracts_table_columns() {
        let entity = parse_fixture("table/config/lookups.ddl");
        let table_def = entity.table_def.as_ref().unwrap();
        assert!(table_def.columns.len() >= 8);

        let id_col = table_def.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id_col.is_pk);
        assert!(!id_col.nullable);
        assert!(id_col.default_value.is_some());

        // Postgres's own spelling of the type it resolved, not the uppercased
        // echo of the author's keyword sqlparser used to report. Every
        // comparison dbd makes runs both sides through `canonical_type`, so the
        // two agree; this is the spelling that now reaches emitted DDL and DBML.
        let name_col = table_def.columns.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.data_type, "varchar(30)");
    }

    #[test]
    fn extracts_table_with_fk_references() {
        let entity = parse_fixture("table/config/lookup_values.ddl");
        let refers: Vec<&str> = entity.refers.iter().map(|s| s.as_str()).collect();
        // Should reference lookups and categories via FK
        assert!(refers.contains(&"config.lookups") || refers.contains(&"lookups"));
    }

    #[test]
    fn extracts_view_references() {
        let entity = parse_fixture("view/config/genders.ddl");
        assert_eq!(entity.entity_type, EntityType::View);
        assert_eq!(entity.name, "config.genders");
        // View references tables it SELECTs from
        assert!(!entity.references.is_empty());
    }

    #[test]
    fn extracts_enum_values() {
        let entity = parse_fixture("enum/config/status.sql");
        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "config.status");
        let values: Vec<&str> = entity.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(values, vec!["active", "inactive", "archived"]);
    }

    #[test]
    fn extracts_procedure_reads_writes() {
        let entity = parse_fixture("procedure/staging/import_lookups.ddl");
        assert_eq!(entity.entity_type, EntityType::Procedure);
        assert!(!entity.reads.is_empty());
        assert!(!entity.writes.is_empty());
        // Reads from staging.lookups, writes to config.lookups
        assert!(entity.reads.iter().any(|r| r.contains("staging.lookups")));
        assert!(entity.writes.iter().any(|w| w.contains("config.lookups")));
    }

    #[test]
    fn handles_parse_error_gracefully() {
        let entity = parse_entity(Path::new("ddl/table/config/broken.ddl"), "THIS IS NOT SQL AT ALL ;;;").unwrap();
        assert!(!entity.errors.is_empty());
    }

    // ── sqlparser 0.62 grammar gaps ──────────────────────────────────────────
    //
    // 0.61 rejected these three, and a parse error is not cosmetic: it drops the
    // entity from apply/reconcile's desired set, so `dbd apply` reported success
    // while never creating the object. Pin them so a future downgrade or a
    // regression in the dependency is caught here rather than in a user's
    // database. `returns setof` is the case that surfaced the whole class.

    #[test]
    fn setof_returning_function_parses_and_keeps_its_refs() {
        let entity = parse_entity(
            Path::new("ddl/function/app/srf.ddl"),
            "set search_path to app;\n\
             create function srf() returns setof t language sql as $$ select * from t $$;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
        assert!(
            entity.reads.contains(&"app.t".to_string()),
            "a parsed function must resolve its read against the file's search_path, got {:?}",
            entity.reads
        );
    }

    #[test]
    fn variadic_function_parses() {
        let entity = parse_entity(
            Path::new("ddl/function/app/vf.ddl"),
            "set search_path to app;\n\
             create function vf(variadic a int[]) returns int language sql as $$ select 1 $$;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
    }

    // ── libpg_query validation fallback ──────────────────────────────────────
    //
    // sqlparser is a convenience parser, not Postgres's. When it rejects a file
    // that libpg_query — Postgres's own grammar — accepts, the limitation is
    // ours and the file is valid SQL. Recording a parse error there is not
    // cosmetic: it drops the entity from apply/reconcile's desired set, so the
    // object is never created and the command still exits 0.
    //
    // The fallback is deliberately NOT applied to tables: a table with no
    // `table_def` is filtered out of the desired snapshot (`reconcile.rs:91`),
    // which makes the live table read as an orphan that `--prune` would DROP.
    // Erroring keeps it visible; silently accepting it risks data loss.

    #[test]
    fn window_function_valid_in_postgres_is_not_an_error() {
        let entity = parse_entity(
            Path::new("ddl/function/app/wf.ddl"),
            "set search_path to app;\n\
             create function wf() returns int language plpgsql as $$ begin perform 1 from t; end $$ window;",
        )
        .unwrap();
        assert!(
            entity.errors.is_empty(),
            "valid Postgres must not be reported as a user error: {:?}",
            entity.errors
        );
    }

    #[test]
    fn view_with_check_option_is_not_an_error_and_keeps_its_refs() {
        let entity = parse_entity(
            Path::new("ddl/view/app/v.ddl"),
            "set search_path to app;\n\
             create view v as select * from t where a > 1 with cascaded check option;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
        assert!(
            entity.refers.contains(&"app.t".to_string()),
            "a recovered view must keep its dependency edge, got {:?}",
            entity.refers
        );
    }

    // The wrong-schema trap: without search_path recovery the fallback would
    // qualify reads to `public`, turning a missing edge into a plausibly-wrong
    // one that points at a different table.

    #[test]
    fn fallback_recovers_search_paths_so_refs_resolve_to_the_right_schema() {
        let entity = parse_entity(
            Path::new("ddl/function/app/wf2.ddl"),
            "set search_path to app;\n\
             create function wf2() returns int language plpgsql as $$ begin perform 1 from t; end $$ window;",
        )
        .unwrap();
        assert_eq!(entity.search_paths, vec!["app".to_string()]);
        assert!(
            entity.reads.contains(&"app.t".to_string()),
            "read must qualify against the file's search_path, not `public`: {:?}",
            entity.reads
        );
    }

    #[test]
    fn table_sqlparser_cannot_read_still_errors() {
        let entity = parse_entity(
            Path::new("ddl/table/app/excl.ddl"),
            "set search_path to app;\n\
             create table excl (id int primary key, r int4range, exclude using gist (r with &&));",
        )
        .unwrap();
        assert!(
            !entity.errors.is_empty(),
            "a table with no table_def reads as an orphan that --prune would drop; \
             it must stay visible as an error"
        );
        assert!(entity.table_def.is_none());
    }

    #[test]
    fn sql_neither_parser_accepts_still_errors() {
        let entity = parse_entity(Path::new("ddl/view/app/broken.ddl"), "create view v as SELECT * FROM ;").unwrap();
        assert!(!entity.errors.is_empty(), "genuinely broken SQL must still error");
    }

    // ── Guarded enum: `DO $$ … $$` around CREATE TYPE ────────────────────────
    //
    // Postgres has no `CREATE TYPE IF NOT EXISTS`, so wrapping the CREATE in a
    // DO block that swallows `duplicate_object` is the only idiom for a
    // conditional enum. sqlparser rejects `DO` outright, and a parse error drops
    // the entity from apply/reconcile's desired set — so the type was never
    // created and the first table using it died with `type "…" does not exist`,
    // never mentioning the real cause. libpg_query reads the block.

    #[test]
    fn guarded_do_block_enum_is_parsed() {
        let sql = "set search_path to app;\n\
                   \n\
                   do $$ begin\n\
                     create type status_t as enum ('active', 'archived');\n\
                   exception when duplicate_object then null;\n\
                   end $$;\n";
        let entity = parse_entity(Path::new("ddl/enum/app/status_t.ddl"), sql).unwrap();

        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "app.status_t");
        assert!(
            entity.errors.is_empty(),
            "a guarded enum is valid Postgres and must not report a parse error: {:?}",
            entity.errors
        );
        let values: Vec<&str> = entity.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(values, vec!["active", "archived"]);

        // The guarded form must record its search path like the plain
        // `create type` form does — this arm returns before the extraction
        // further down, so it has to set it itself.
        assert_eq!(entity.search_paths, vec!["app".to_string()]);
    }

    // The fallback must not turn every unparseable enum file into a silent pass.

    #[test]
    fn unparseable_enum_still_reports_an_error() {
        let entity = parse_entity(Path::new("ddl/enum/app/broken.ddl"), "THIS IS NOT SQL AT ALL ;;;").unwrap();
        assert!(
            !entity.errors.is_empty(),
            "a broken enum file must still surface a parse error"
        );
        assert!(entity.enum_values.is_empty());
    }

    #[test]
    fn do_block_declaring_no_enum_keeps_its_parse_error() {
        let entity = parse_entity(Path::new("ddl/enum/app/empty.ddl"), "do $$ begin perform 1; end $$;").unwrap();
        assert!(
            !entity.errors.is_empty(),
            "a DO block with no CREATE TYPE declares no enum — the error must stand"
        );
        assert!(entity.enum_values.is_empty());
    }

    // ── Role round-trip: emit → parse → refers ───────────────────────────────
    //
    // Proves that generate_role_script output is correctly parsed back, so that
    // role memberships survive a dbd apply cycle.
    #[test]
    fn role_membership_round_trip() {
        use crate::entity::EntityType;
        use crate::script::ddl_from_entity;

        // Build a role entity with two parent memberships.
        let mut role = crate::entity::Entity::new(EntityType::Role, "app_ro");
        role.refers = vec!["app_admin".to_string(), "other_parent".to_string()];

        // Emit DDL via the existing Role arm of ddl_from_entity.
        let emitted = ddl_from_entity(&role).expect("ddl_from_entity must return Some for Role");

        // The emitted text should contain the GRANT lines.
        assert!(
            emitted.contains("GRANT \"app_admin\" TO \"app_ro\""),
            "emitted DDL missing app_admin grant:\n{emitted}"
        );
        assert!(
            emitted.contains("GRANT \"other_parent\" TO \"app_ro\""),
            "emitted DDL missing other_parent grant:\n{emitted}"
        );

        // Now parse the emitted DDL back — this is the path `dbd apply` takes
        // when it re-reads a file written by `dbd merge --roles`.
        let parsed =
            parse_entity(Path::new("ddl/role/app_ro.ddl"), &emitted).expect("parse_entity must not error on role DDL");

        assert_eq!(parsed.entity_type, EntityType::Role);
        assert_eq!(parsed.name, "app_ro");

        // Both parent roles must survive the round-trip.
        assert!(
            parsed.refers.contains(&"app_admin".to_string()),
            "app_admin missing from parsed refers: {:?}",
            parsed.refers
        );
        assert!(
            parsed.refers.contains(&"other_parent".to_string()),
            "other_parent missing from parsed refers: {:?}",
            parsed.refers
        );
        assert_eq!(parsed.refers.len(), 2, "unexpected extra refers: {:?}", parsed.refers);
    }

    #[test]
    fn role_with_no_grants_has_empty_refers() {
        use crate::entity::EntityType;
        use crate::script::ddl_from_entity;

        let role = crate::entity::Entity::new(EntityType::Role, "basic");
        let emitted = ddl_from_entity(&role).unwrap();

        let parsed = parse_entity(Path::new("ddl/role/basic.ddl"), &emitted).unwrap();
        assert!(
            parsed.refers.is_empty(),
            "role with no grants should have empty refers, got {:?}",
            parsed.refers
        );
    }

    #[test]
    fn role_bare_identifier_grant_parsed() {
        // Hand-authored files may omit double-quotes; the regex must handle bare identifiers.
        let sql = "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'child') THEN\n    CREATE ROLE \"child\";\n  END IF;\nEND $$;\nGRANT parent TO child;\n";
        let parsed = parse_entity(Path::new("ddl/role/child.ddl"), sql).unwrap();
        assert!(
            parsed.refers.contains(&"parent".to_string()),
            "bare-identifier grant not parsed; refers: {:?}",
            parsed.refers
        );
    }

    #[test]
    fn extracts_matview_body_and_indexes() {
        let sql = "CREATE MATERIALIZED VIEW analytics.daily_sales AS\n\
                   SELECT date_trunc('day', created_at) AS day, sum(total) AS revenue\n\
                   FROM shop.orders GROUP BY 1 WITH DATA;\n\
                   CREATE UNIQUE INDEX daily_sales_day_uidx ON analytics.daily_sales(day);";
        let entity = parse_entity(Path::new("ddl/materialized_view/analytics/daily_sales.ddl"), sql).unwrap();

        assert_eq!(entity.entity_type, EntityType::MaterializedView);
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);

        // Body captured the same way a view's body is (verbatim in writes[0]).
        let body = entity.writes.first().expect("matview body should be captured");
        assert!(
            body.to_lowercase().contains("from shop.orders"),
            "body missing source table: {body}"
        );

        // Trailing CREATE INDEX captured like a table's indexes.
        let indexes = &entity
            .table_def
            .as_ref()
            .expect("matview should have a table_def")
            .indexes;
        assert_eq!(indexes.len(), 1, "expected exactly one index: {indexes:?}");
        assert!(indexes[0].unique, "expected a UNIQUE index");
    }

    /// `COMMENT ON MATERIALIZED VIEW` used to need a text-stripping workaround,
    /// because sqlparser only understood `COMMENT ON TABLE`/`COLUMN` and a parse
    /// error drops the entity from the desired set. libpg_query reads it, so the
    /// workaround is gone — this asserts the property the workaround protected
    /// rather than the workaround itself, which is why it survives its deletion.
    #[test]
    fn comment_on_materialized_view_does_not_break_the_file() {
        let sql = "CREATE MATERIALIZED VIEW analytics.daily_sales AS SELECT 1 AS n FROM shop.orders;\n\
                   COMMENT ON MATERIALIZED VIEW analytics.daily_sales IS 'daily rollup';";
        let entity = parse_entity(Path::new("ddl/materialized_view/analytics/daily_sales.ddl"), sql).unwrap();

        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
        assert!(
            entity.refers.contains(&"shop.orders".to_string()),
            "the matview must keep its dependency edge, got {:?}",
            entity.refers
        );
    }

    // ── ParserChoice ────────────────────────────────────────────────────────

    #[test]
    fn postgres_dialects_default_to_pg_query() {
        assert_eq!(
            ParserChoice::resolve("postgresql", None).unwrap(),
            ParserChoice::PgQuery
        );
        assert_eq!(ParserChoice::resolve("supabase", None).unwrap(), ParserChoice::PgQuery);
        // `dbd doctor --fix` migrates a legacy `project.database: Postgres`
        // to this spelling (doctor.rs:120-127), so it must not miss.
        assert_eq!(ParserChoice::resolve("postgres", None).unwrap(), ParserChoice::PgQuery);
    }

    #[test]
    fn an_explicit_parser_is_accepted_whatever_the_dialect() {
        assert_eq!(
            ParserChoice::resolve("sqlite", Some("pg_query")).unwrap(),
            ParserChoice::PgQuery
        );
    }

    // ── ParserChoice after the sqlparser retirement ─────────────────────────
    //
    // `sqlparser` was never a dialect — `parse_with_sqlparser` hardcoded
    // `PostgreSqlDialect`, so it was a second *Postgres* parser kept alive as
    // an escape hatch during the libpg_query migration. Every file-backed type
    // is now native (`pg::PgQueryDdl::COVERED`), which is the precondition the
    // migration spec named for retiring it.

    /// A project still naming the retired parser must be told it is gone, not
    /// silently switched to a different one — the same reasoning that already
    /// makes an unrecognised value an error rather than a fallback.
    #[test]
    fn sqlparser_is_rejected_by_name_and_says_it_was_removed() {
        let err = ParserChoice::resolve("postgresql", Some("sqlparser"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sqlparser"), "must name the value it rejects: {err}");
        assert!(
            err.contains("removed"),
            "must say it was removed, not merely that it is invalid: {err}"
        );
        assert!(err.contains("pg_query"), "must name the remaining valid value: {err}");
    }

    /// No non-Postgres grammar is wired, and none ever was: `init --from-db
    /// sqlite://` writes no `source:` block (`reverse::design_yaml`), so every
    /// generated SQLite project has always loaded under the `postgresql`
    /// default and reached `PgQuery`. This arm keeps that true instead of
    /// naming a parser that no longer exists.
    #[test]
    fn a_non_postgres_dialect_gets_the_postgres_parser() {
        assert_eq!(ParserChoice::resolve("sqlite", None).unwrap(), ParserChoice::PgQuery);
    }

    /// `source.parser` is public API, so a typo must not silently leave the
    /// project on a parser the author did not ask for.
    #[test]
    fn an_unknown_parser_errors_and_names_the_valid_values() {
        let err = ParserChoice::resolve("postgresql", Some("pgquery"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("pg_query"), "got: {err}");

        assert!(ParserChoice::resolve("postgresql", Some("")).is_err());
        assert!(ParserChoice::resolve("postgresql", Some("PG_QUERY")).is_err());
    }

    // ── DdlParser ───────────────────────────────────────────────────────────

    /// Object safety is a real requirement: dispatch selects an implementation
    /// at runtime, so the trait must be usable behind a reference. It matters
    /// more now than when there were two implementations, not less — this is
    /// what keeps the seam usable for the non-Postgres grammar that would be
    /// added against it.
    #[test]
    fn pg_query_ddl_is_usable_as_a_trait_object() {
        let parser: &dyn DdlParser = &pg::PgQueryDdl;
        let entity = parser
            .parse(Path::new("ddl/enum/app/s.ddl"), "create type s as enum ('a', 'b');")
            .unwrap();
        assert_eq!(entity.enum_values.len(), 2);
        assert!(entity.errors.is_empty(), "got: {:?}", entity.errors);
    }
}
