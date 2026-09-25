mod dialect;
pub(crate) mod pg;

pub use dialect::Dialect;

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};

/// Which reader dbd runs over a file.
///
/// Not the same question as [`Dialect`], which is what the SQL *is*. Several
/// dialects can share a reader, and a dialect dbd has no reader for still has a
/// name — [`Self::for_dialect_typed`] is the one place one becomes the other.
///
/// Two variants, for the two shapes of model dbd has: a structured one that
/// `reconcile` can diff, and the file's own text for targets where that text is
/// the schema. It was two during the libpg_query migration as well, but for a
/// different reason — `Sqlparser` held a second implementation as an escape
/// hatch, and that implementation hardcoded `PostgreSqlDialect`, so it was
/// never a dialect selector at all. It retired once every file-backed type
/// became native (see `pg::PgQueryDdl::COVERED`, or [`pg_native_types`]).
///
/// Serializes as the value `source.parser` accepts (`pg_query`, `verbatim`), so
/// a reported choice round-trips back into a config rather than needing a
/// second mapping to be invented at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParserChoice {
    /// libpg_query — PostgreSQL's own grammar, vendored from the server.
    /// Produces a fully structured [`Entity`]: columns, constraints, indexes.
    PgQuery,
    /// The file is the model. Identity comes from the path; the SQL is kept
    /// verbatim in [`Entity::raw_ddl`] and applied as written.
    ///
    /// This is not a weaker fallback, it is the shape SQLite already has on the
    /// other side: `SqliteAdapter::introspect` builds each entity from
    /// `sqlite_master.sql` with `raw_ddl` and no `table_def`, because that text
    /// *is* the schema — losslessly, `AUTOINCREMENT` and `WITHOUT ROWID` and
    /// `STRICT` included. Reading a SQLite project's files any other way would
    /// make the two sides disagree about what a table even is.
    Verbatim,
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
            Some("verbatim") => Ok(Self::Verbatim),
            Some("sqlparser") => Err(DbdError::Config(
                "source.parser \"sqlparser\" was removed — it was a second PostgreSQL parser, \
                 not a dialect, and every entity type is now read by libpg_query. \
                 Drop the line, or set \"pg_query\"."
                    .to_string(),
            )),
            Some(other) => Err(DbdError::Config(format!(
                "unknown source.parser {other:?} — expected \"pg_query\" or \"verbatim\""
            ))),
            None => Ok(Self::for_dialect(dialect)),
        }
    }

    /// The parser a `source.dialect` label selects when `source.parser` is
    /// unset.
    ///
    /// An *unrecognised* label is deliberately not an error here. It would be a
    /// breaking change for a value someone already has in a working project,
    /// and the failure it would prevent surfaces anyway the moment the reader
    /// rejects a file — with the offending SQL named, which is more use than a
    /// complaint about a config string.
    fn for_dialect(label: &str) -> Self {
        match Dialect::from_label(label) {
            Some(d) => Self::for_dialect_typed(d),
            None => Self::PgQuery,
        }
    }

    /// The reader a dialect is read by.
    ///
    /// The single mapping from *what the SQL is* to *how dbd reads it*. Both
    /// the stated path ([`Self::resolve`], from `source.dialect`) and the
    /// detected path ([`Dialect::detect`]) go through here, so a config label
    /// and a detected dialect can never select different readers for the same
    /// SQL.
    ///
    /// `Sqlite` reads verbatim, matching how its own adapter models a table.
    /// [`Dialect::Unstated`] falls back to libpg_query — it has no reader of
    /// its own, and a file that reader cannot read reports why, which is the
    /// one thing a caller can rely on.
    pub fn for_dialect_typed(dialect: Dialect) -> Self {
        match dialect {
            Dialect::Sqlite => Self::Verbatim,
            Dialect::PostgreSql | Dialect::TSql | Dialect::MySql | Dialect::Unstated => Self::PgQuery,
        }
    }
}

/// Reads a DDL file into an [`Entity`].
///
/// Two implementations, and they differ in what an entity *is* rather than in
/// which grammar reads it: [`pg::PgQueryDdl`] produces a structured model that
/// reconcile can diff, [`VerbatimDdl`] produces the file's own text because for
/// its targets that text is the schema. A third would be a real non-Postgres
/// grammar — not the sqlparser twin that used to sit here, which was a second
/// Postgres parser wearing a dialect's name.
pub(crate) trait DdlParser {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity>;
}

/// Takes the file as the model: identity from the path, SQL kept verbatim.
///
/// No grammar is involved, so nothing about the SQL can make it fail. That is
/// correct for a target whose own catalog hands back `CREATE` text — see
/// [`ParserChoice::Verbatim`] — and would be wrong for one dbd diffs
/// structurally, which is why the choice is made once from the dialect rather
/// than per file.
pub(crate) struct VerbatimDdl;

impl DdlParser for VerbatimDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        let mut entity = Entity::from_file(file);
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            // An empty file declares nothing. Erroring keeps it visible rather
            // than contributing an entity that applies no SQL.
            entity.errors.push("this file is empty".to_string());
            return Ok(entity);
        }
        entity.raw_ddl = Some(trimmed.to_string());
        Ok(entity)
    }
}

/// Parse a DDL file with an explicit parser choice.
///
/// The project scan (`design::from_config_with_dir`) resolves `source.parser`
/// once, before reading any file, and calls this directly so a bad config
/// value fails at load rather than partway through the scan.
pub fn parse_entity_with(choice: ParserChoice, file: &Path, sql: &str) -> Result<Entity> {
    match choice {
        ParserChoice::PgQuery => pg::PgQueryDdl.parse(file, sql),
        ParserChoice::Verbatim => VerbatimDdl.parse(file, sql),
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

/// Everything one SQL file declares.
///
/// The return of [`parse_sql`]. Holds dbd's own [`Entity`] rather than a
/// reduced, indexer-shaped type: the read/write split on routines and the
/// soft/hard distinction on references are the parts an embedder cannot get
/// from any other language's parser, and flattening them here would throw away
/// the reason to call this at all.
/// What a SQL file is *for*.
///
/// A corpus is mostly not declarations. Measured over 2,154 real T-SQL files,
/// and independently by sensei over its own corpus, `ALTER TABLE` outnumbers
/// `CREATE TABLE` 159 to 101 — the commonest statement in a SQL codebase
/// declares nothing at all.
///
/// That matters to a caller building a graph. A change script that minted an
/// identity for the table it alters produces two nodes for one table; a data
/// script that minted one produces a node for a table defined elsewhere. This
/// is what tells "owns the entity" from "touches it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Declares entities and changes nothing it does not declare. A dbd DDL
    /// file, and the shape `Entity` was built for. Its own indexes and
    /// comments count as part of the declaration.
    Declaration,
    /// Changes objects defined elsewhere — `ALTER`, `DROP`, an index on
    /// somebody else's table. Declares nothing, so it contributes edges rather
    /// than nodes.
    Migration,
    /// Moves rows, not shapes.
    Data,
    /// Declares something *and* changes something else, or mixes data in.
    Mixed,
    /// Nothing dbd recognises — a bare `SELECT`, a file of comments.
    #[default]
    Empty,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFile {
    /// What the file is for — see [`FileKind`].
    pub kind: FileKind,
    /// The dialect the file was read as.
    ///
    /// [`Dialect::Unstated`] when nothing identified it: it was still read, by
    /// the fallback reader, but saying `PostgreSql` would claim the file stated
    /// something it did not.
    pub dialect: Dialect,
    /// One per declaration, in source order. Empty when the file declares
    /// nothing — a migration that only `INSERT`s is not an error.
    pub entities: Vec<Entity>,
    /// The file's `SET search_path`, or `["public"]`. Unqualified names in
    /// `entities` were resolved against its first element, and the full list is
    /// the candidate set for resolving the rest (see
    /// [`crate::references::resolve_references`]).
    pub search_paths: Vec<String>,
    /// File-level failures — SQL Postgres itself rejects. Per-entity problems
    /// stay on `Entity::errors`.
    pub errors: Vec<String>,
}

/// Read every entity a SQL file declares, taking identity from the statements.
///
/// The counterpart to [`parse_entity`], for callers that are not inside dbd's
/// `ddl/<type>/<schema>/<name>.ddl` layout. `parse_entity` derives type, schema
/// and name from the path, and outside that layout it does not fail — it falls
/// back to [`EntityType::Table`] and names the entity after a directory, so a
/// stored procedure reads as a table and only `entity.errors` hints otherwise.
/// This asks the SQL instead.
///
/// Resolution is deliberately *not* done here. Each entity carries its
/// references as written, provisionally qualified against `search_paths[0]`,
/// and [`crate::references::resolve_references`] re-resolves them once every
/// file has been read. That split is what makes a scan parallelisable: this
/// function touches no shared state, so a bare `t` that could be `a.t` or `b.t`
/// stays undecided until the whole set is known, rather than forcing the
/// scanner to be sequential.
///
/// Defaults to PostgreSQL; use [`parse_sql_with`] to choose.
pub fn parse_sql(sql: &str) -> Result<ParsedFile> {
    parse_sql_as(Dialect::PostgreSql, sql)
}

/// [`parse_sql`] for a known — or deliberately unknown — dialect.
///
/// The entry point for a multi-dialect scan: pair it with [`Dialect::detect`]
/// for a file nothing states, and the result records `Unstated` rather than
/// claiming the fallback reader's dialect as the file's own.
///
/// ```no_run
/// # fn example(sql: &str) -> dbd_core::Result<()> {
/// use dbd_core::parser::{Dialect, parse_sql_as};
///
/// let parsed = parse_sql_as(Dialect::detect(sql), sql)?;
/// println!("{:?} file, read as {:?}", parsed.kind, parsed.dialect);
/// # Ok(())
/// # }
/// ```
pub fn parse_sql_as(dialect: Dialect, sql: &str) -> Result<ParsedFile> {
    let mut parsed = parse_sql_with(ParserChoice::for_dialect_typed(dialect), sql)?;
    parsed.dialect = dialect;
    Ok(parsed)
}

/// [`parse_sql`] with an explicit parser choice.
///
/// Pair with [`ParserChoice::resolve`] to derive the choice from a dialect
/// string, which is what the project scan does for `source.dialect`.
///
/// [`ParserChoice::Verbatim`] is an error here rather than an empty result.
/// Statement-level identity is exactly what that path does not have — it never
/// looks at the SQL — so answering "this file declares nothing" would be a
/// claim about content, made without reading any.
pub fn parse_sql_with(choice: ParserChoice, sql: &str) -> Result<ParsedFile> {
    match choice {
        ParserChoice::PgQuery => pg::parse_sql(sql),
        ParserChoice::Verbatim => Err(DbdError::Config(
            "parse_sql needs a grammar, and the verbatim parser has none — it takes identity \
             from the file path, not the statements. Use `parse_entity` for a verbatim project, \
             or pass `ParserChoice::PgQuery` to read PostgreSQL DDL."
                .to_string(),
        )),
    }
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

    /// SQLite models a table as its `CREATE` text — `SqliteAdapter::introspect`
    /// sets `raw_ddl` and no `table_def` — so reading a SQLite project's files
    /// through libpg_query was never right: it rejects `AUTOINCREMENT`,
    /// `WITHOUT ROWID` and `STRICT` outright, and a project `init --from-db`
    /// had just written refused to load (issue #20).
    #[test]
    fn the_sqlite_dialect_selects_the_verbatim_parser() {
        assert_eq!(ParserChoice::resolve("sqlite", None).unwrap(), ParserChoice::Verbatim);
    }

    /// An unrecognised dialect still gets libpg_query rather than an error.
    /// Erroring would break a value someone already has in a working project,
    /// and a file libpg_query cannot read reports the offending SQL — more use
    /// than a complaint about a config string.
    #[test]
    fn an_unrecognised_dialect_falls_back_to_the_postgres_parser() {
        assert_eq!(ParserChoice::resolve("mysql", None).unwrap(), ParserChoice::PgQuery);
        assert_eq!(ParserChoice::resolve("", None).unwrap(), ParserChoice::PgQuery);
    }

    /// The verbatim parser takes the file as written and asks no grammar
    /// anything — which is what makes it usable for SQLite DDL that libpg_query
    /// rejects.
    #[test]
    fn the_verbatim_parser_keeps_sql_no_postgres_grammar_accepts() {
        let sql = "CREATE TABLE settings (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;";
        assert!(
            pg_query::parse(sql).is_err(),
            "precondition: libpg_query must reject this, or the test proves nothing"
        );

        let entity = parse_entity_with(ParserChoice::Verbatim, Path::new("ddl/table/settings.ddl"), sql).unwrap();
        assert!(entity.errors.is_empty(), "unexpected errors: {:?}", entity.errors);
        assert_eq!(entity.entity_type, EntityType::Table);
        assert_eq!(entity.name, "settings", "SQLite is schema-less, so the name stays bare");
        assert_eq!(entity.schema, None);
        assert_eq!(entity.raw_ddl.as_deref(), Some(sql));
        assert!(
            entity.table_def.is_none(),
            "the verbatim path models no structure — the text is the model"
        );
    }

    /// An empty file declares nothing. Erroring keeps it visible rather than
    /// contributing an entity that applies no SQL.
    #[test]
    fn the_verbatim_parser_rejects_an_empty_file() {
        let entity = parse_entity_with(ParserChoice::Verbatim, Path::new("ddl/table/blank.ddl"), "  \n\t ").unwrap();
        assert!(!entity.errors.is_empty(), "an empty file must not pass silently");
        assert!(entity.raw_ddl.is_none());
    }

    /// `parse_sql` is statement-level, and the verbatim path never looks at a
    /// statement. Answering "declares nothing" would be a claim about content
    /// made without reading any, so it refuses instead.
    #[test]
    fn parse_sql_refuses_the_verbatim_parser_rather_than_returning_empty() {
        let err = parse_sql_with(ParserChoice::Verbatim, "create table t (a int);")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("parse_entity"),
            "must point at the usable entry point: {err}"
        );
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
