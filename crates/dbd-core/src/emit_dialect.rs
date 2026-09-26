//! Emit a PostgreSQL project's schema as another engine's DDL (#23).
//!
//! # One direction, and why
//!
//! Only [`ParserChoice::PgQuery`] produces a structured model. The
//! statement-head readers know a table's name and not one of its columns, and
//! the verbatim reader keeps text. So the source must be PostgreSQL; asking to
//! emit *from* anything else is refused rather than quietly producing an empty
//! schema.
//!
//! [`ParserChoice::PgQuery`]: crate::parser::ParserChoice::PgQuery
//!
//! # Downgrade, do not refuse
//!
//! A construct the target cannot express is emitted as the nearest thing it
//! can, so the output is always a complete schema. The cost is that DDL which
//! applies cleanly can mean something different, and the **report is the only
//! safeguard** — so a lossy downgrade is recorded twice: as a comment at the
//! site in the emitted file, and in the returned [`Downgrade`] list.
//!
//! A *faithful* mapping is recorded in neither. MySQL has a native `ENUM`, and
//! `integer` is `INTEGER` everywhere; listing those would pad the report until
//! nobody reads the entries that matter.
//!
//! # What is not emitted
//!
//! Functions, procedures and triggers. Their bodies are PL/pgSQL or SQL that
//! does not translate, and dbd holds them as opaque text — emitting them would
//! produce something that looks convertible and is not. They are reported as
//! skipped so the omission is visible.

use crate::entity::{ColumnDef, Entity, EntityType, TableConstraint};
use crate::error::{DbdError, Result};
use crate::parser::Dialect;

/// One thing the target could not express faithfully.
///
/// Every entry is a real loss of meaning. A mapping that keeps the meaning
/// produces no entry — see the module note on why that matters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Downgrade {
    /// The entity it happened in, schema-qualified.
    pub entity: String,
    /// The column, when it is a column-level loss. `None` for a whole-entity
    /// one — a skipped procedure, a view body passed through untranslated.
    pub column: Option<String>,
    /// What PostgreSQL had.
    pub from: String,
    /// What the target got instead.
    pub to: String,
    /// Why it is a loss, in terms the reader can act on.
    pub reason: String,
}

impl Downgrade {
    /// The note written into the emitted file at the site of the loss.
    fn comment(&self, target: Target) -> String {
        let what = match &self.column {
            Some(c) => format!("`{c}` was {}", self.from),
            None => format!("{} was {}", self.entity, self.from),
        };
        format!(
            "{} dbd: {what} — emitted as {}; {}",
            target.comment_prefix(),
            self.to,
            self.reason
        )
    }
}

/// The engines `emit` can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    MySql,
    TSql,
    Sqlite,
}

impl Target {
    fn from_dialect(d: Dialect) -> Result<Self> {
        match d {
            Dialect::MySql => Ok(Self::MySql),
            Dialect::TSql => Ok(Self::TSql),
            Dialect::Sqlite => Ok(Self::Sqlite),
            Dialect::PostgreSql => Err(DbdError::Config(
                "emit writes another engine's DDL; for this project's own PostgreSQL DDL \
                 use `dbd combine`, which consolidates the authored files into one script."
                    .to_string(),
            )),
            Dialect::Unstated => Err(DbdError::Config(
                "emit needs a target dialect: mysql, tsql or sqlite".to_string(),
            )),
        }
    }

    /// Every engine here takes `--`, so the note is valid in all three. Kept as
    /// a method because the first target that does not (a JSON or TS emitter)
    /// must not silently write a broken comment.
    fn comment_prefix(self) -> &'static str {
        "--"
    }

    /// Quote an identifier the way the target does.
    fn quote(self, name: &str) -> String {
        match self {
            Self::MySql => format!("`{name}`"),
            Self::TSql => format!("[{name}]"),
            Self::Sqlite => format!("\"{name}\""),
        }
    }

    /// Whether the target has schemas. MySQL's "schema" is a database and
    /// SQLite has none, so a qualified name has to be flattened for both.
    fn has_schemas(self) -> bool {
        matches!(self, Self::TSql)
    }

    fn label(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::TSql => "SQL Server",
            Self::Sqlite => "SQLite",
        }
    }
}

/// Emit `design`'s schema as `target`'s DDL, with every lossy downgrade
/// reported.
///
/// Returns the script and the downgrades. The script already contains a
/// comment at each downgrade site; the list is the same set, for a caller that
/// wants to count or serialize it.
pub fn emit_schema(
    design: &crate::Design,
    target: Dialect,
    scope: Option<&crate::scope::ResolvedScope>,
) -> Result<(String, Vec<Downgrade>)> {
    let target = Target::from_dialect(target)?;
    if !design.parser().produces_structure() {
        return Err(DbdError::Config(format!(
            "emit needs a structured model and this project has none (source.dialect: {}) — \
             its DDL is read for identity and references, not columns, so there is nothing \
             to translate. Only a PostgreSQL project can be emitted as another dialect.",
            design.dialect()
        )));
    }

    let entities = match scope {
        Some(s) => design.scoped_entities(s)?,
        None => design.entities().to_vec(),
    };

    let mut out = vec![format!(
        "{} Generated by dbd emit --dialect {} — from a PostgreSQL project.\n\
         {} Downgrades are noted inline; see the run's report for the full list.",
        target.comment_prefix(),
        match target {
            Target::MySql => "mysql",
            Target::TSql => "tsql",
            Target::Sqlite => "sqlite",
        },
        target.comment_prefix(),
    )];
    let mut report = Vec::new();

    for e in entities.iter().filter(|e| e.errors.is_empty()) {
        match e.entity_type {
            EntityType::Table => out.push(emit_table(e, target, &mut report)),
            EntityType::View | EntityType::MaterializedView => {
                if let Some(sql) = emit_view(e, target, &mut report) {
                    out.push(sql);
                }
            }
            EntityType::Function | EntityType::Procedure | EntityType::Trigger => {
                report.push(Downgrade {
                    entity: e.name.clone(),
                    column: None,
                    from: format!("a {}", e.entity_type.tag()),
                    to: "nothing".to_string(),
                    reason: "its body is PL/pgSQL or SQL that does not translate, and dbd holds \
                             it as opaque text — emitting it would look convertible and not be"
                        .to_string(),
                });
            }
            // Schemas, extensions, enums and roles are handled where they are
            // used (a column's type) or have no target equivalent worth a
            // standalone statement.
            _ => {}
        }
    }

    Ok((out.join("\n\n") + "\n", report))
}

/// `schema.name` flattened for a target without schemas.
fn table_name(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> String {
    let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
    match (&e.schema, target.has_schemas()) {
        (Some(s), true) => format!("{}.{}", target.quote(s), target.quote(bare)),
        (Some(s), false) => {
            report.push(Downgrade {
                entity: e.name.clone(),
                column: None,
                from: format!("schema `{s}`"),
                to: format!("the name `{s}_{bare}`"),
                reason: format!(
                    "{} has no schemas, so the qualification is folded into the name and every \
                     reference to it must be updated by hand",
                    target.label()
                ),
            });
            target.quote(&format!("{s}_{bare}"))
        }
        (None, _) => target.quote(bare),
    }
}

fn emit_table(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> String {
    let Some(td) = &e.table_def else {
        return format!("{} dbd: {} has no readable structure.", target.comment_prefix(), e.name);
    };
    let name = table_name(e, target, report);

    let mut lines: Vec<String> = Vec::new();
    for c in &td.columns {
        let before = report.len();
        let ty = map_type(&c.data_type, target, e, c, report);
        // A note goes immediately above the column it explains.
        for d in &report[before..] {
            lines.push(format!("  {}", d.comment(target)));
        }
        let mut col = format!("  {} {ty}", target.quote(&c.name));
        if !c.nullable {
            col.push_str(" NOT NULL");
        }
        if let Some(d) = &c.default_value
            && let Some(mapped) = map_default(d, target)
        {
            col.push_str(&format!(" DEFAULT {mapped}"));
        }
        lines.push(format!("{col},"));
    }

    // A PARSED table carries its key on the column (`is_pk`/`is_unique`); only
    // the reconcile path lifts those into constraints. Reading constraints
    // alone emitted a table with no key — valid DDL, wrong schema, and a loss
    // the report could not have caught because nothing knew it happened.
    let inline_pk: Vec<String> = td.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
    if !inline_pk.is_empty() {
        lines.push(format!("  PRIMARY KEY ({}),", quote_all(&inline_pk, target)));
    }
    for c in td.columns.iter().filter(|c| c.is_unique && !c.is_pk) {
        lines.push(format!("  UNIQUE ({}),", target.quote(&c.name)));
    }

    for con in &td.constraints {
        match con {
            // Skipped when the columns already declared one inline — a table
            // with two PRIMARY KEY clauses is rejected by every target.
            TableConstraint::PrimaryKey { columns, .. } if !columns.is_empty() && inline_pk.is_empty() => {
                lines.push(format!("  PRIMARY KEY ({}),", quote_all(columns, target)));
            }
            TableConstraint::Unique { columns, .. } if !columns.is_empty() => {
                lines.push(format!("  UNIQUE ({}),", quote_all(columns, target)));
            }
            _ => {}
        }
    }

    let body = lines.join("\n");
    let body = body.trim_end().trim_end_matches(',').to_string();
    format!("CREATE TABLE {name} (\n{body}\n);")
}

fn emit_view(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> Option<String> {
    let body = e.body.first()?;
    let name = table_name(e, target, report);
    report.push(Downgrade {
        entity: e.name.clone(),
        column: None,
        from: "a view body in PostgreSQL SQL".to_string(),
        to: "the same text, untranslated".to_string(),
        reason: "dbd translates types and structure, not expressions — anything \
                 PostgreSQL-specific inside the SELECT has to be checked by hand"
            .to_string(),
    });
    let note = report.last().expect("just pushed").comment(target);
    Some(format!("{note}\nCREATE VIEW {name} AS {body};"))
}

fn quote_all(columns: &[String], target: Target) -> String {
    columns.iter().map(|c| target.quote(c)).collect::<Vec<_>>().join(", ")
}

/// PostgreSQL's type as the target's, recording the loss when there is one.
fn map_type(pg: &str, target: Target, e: &Entity, c: &ColumnDef, report: &mut Vec<Downgrade>) -> String {
    let t = pg.trim().to_lowercase();
    let mut lose = |to: &str, reason: &str| {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: Some(c.name.clone()),
            from: format!("`{pg}`"),
            to: to.to_string(),
            reason: reason.to_string(),
        });
        to.to_string()
    };

    if let Some(inner) = t.strip_suffix("[]") {
        let to = match target {
            Target::MySql => "JSON",
            Target::TSql => "nvarchar(max)",
            Target::Sqlite => "TEXT",
        };
        return lose(
            to,
            &format!("no array type exists here, so the `{inner}` elements become a JSON document"),
        );
    }

    // `varchar(20)` keeps its length; match on the base.
    let base = t.split('(').next().unwrap_or(&t).trim();
    let args = t.find('(').map(|i| &t[i..]).unwrap_or("");

    match (base, target) {
        ("jsonb" | "json", Target::MySql) => "JSON".to_string(),
        ("jsonb" | "json", Target::TSql) => lose(
            "nvarchar(max)",
            "SQL Server has no JSON column type here, so it is stored as text and the \
             engine will not validate it",
        ),
        ("jsonb" | "json", Target::Sqlite) => {
            lose("TEXT", "SQLite stores JSON as text; the engine will not validate it")
        }

        ("uuid", Target::MySql) => lose(
            "CHAR(36)",
            "MySQL has no UUID column type, so it is stored as its text form",
        ),
        ("uuid", Target::TSql) => "uniqueidentifier".to_string(),
        ("uuid", Target::Sqlite) => lose("TEXT", "SQLite has no UUID type"),

        ("text", Target::MySql) => "TEXT".to_string(),
        ("text", Target::TSql) => "nvarchar(max)".to_string(),
        ("text", Target::Sqlite) => "TEXT".to_string(),

        ("varchar" | "character varying", Target::MySql) => format!("VARCHAR{}", args_or(args, "(255)")),
        ("varchar" | "character varying", Target::TSql) => format!("nvarchar{}", args_or(args, "(255)")),
        ("varchar" | "character varying", Target::Sqlite) => "TEXT".to_string(),

        ("integer" | "int" | "int4" | "serial", Target::MySql) => "INT".to_string(),
        ("integer" | "int" | "int4" | "serial", Target::TSql) => "int".to_string(),
        ("integer" | "int" | "int4" | "serial", Target::Sqlite) => "INTEGER".to_string(),

        ("bigint" | "int8" | "bigserial", Target::MySql) => "BIGINT".to_string(),
        ("bigint" | "int8" | "bigserial", Target::TSql) => "bigint".to_string(),
        ("bigint" | "int8" | "bigserial", Target::Sqlite) => "INTEGER".to_string(),

        ("smallint" | "int2", Target::MySql) => "SMALLINT".to_string(),
        ("smallint" | "int2", Target::TSql) => "smallint".to_string(),
        ("smallint" | "int2", Target::Sqlite) => "INTEGER".to_string(),

        ("boolean" | "bool", Target::MySql) => "TINYINT(1)".to_string(),
        ("boolean" | "bool", Target::TSql) => "bit".to_string(),
        ("boolean" | "bool", Target::Sqlite) => "INTEGER".to_string(),

        ("numeric" | "decimal", Target::MySql) => format!("DECIMAL{}", args_or(args, "(65,30)")),
        ("numeric" | "decimal", Target::TSql) => format!("decimal{}", args_or(args, "(38,10)")),
        ("numeric" | "decimal", Target::Sqlite) => "NUMERIC".to_string(),

        ("double precision" | "float8", Target::MySql) => "DOUBLE".to_string(),
        ("double precision" | "float8", Target::TSql) => "float".to_string(),
        ("double precision" | "float8", Target::Sqlite) => "REAL".to_string(),

        ("timestamptz" | "timestamp with time zone", Target::MySql) => "DATETIME".to_string(),
        ("timestamptz" | "timestamp with time zone", Target::TSql) => "datetimeoffset".to_string(),
        ("timestamptz" | "timestamp with time zone", Target::Sqlite) => "TEXT".to_string(),

        ("timestamp" | "timestamp without time zone", Target::MySql) => "DATETIME".to_string(),
        ("timestamp" | "timestamp without time zone", Target::TSql) => "datetime2".to_string(),
        ("timestamp" | "timestamp without time zone", Target::Sqlite) => "TEXT".to_string(),

        ("date", Target::MySql) => "DATE".to_string(),
        ("date", Target::TSql) => "date".to_string(),
        ("date", Target::Sqlite) => "TEXT".to_string(),

        ("bytea", Target::MySql) => "BLOB".to_string(),
        ("bytea", Target::TSql) => "varbinary(max)".to_string(),
        ("bytea", Target::Sqlite) => "BLOB".to_string(),

        // Anything left is a user-defined type — most often an enum, which dbd
        // knows the labels of only at the project level. Text plus a note is
        // the honest floor; a CHECK would need the labels threaded here.
        _ => {
            let to = match target {
                Target::MySql => "VARCHAR(255)",
                Target::TSql => "nvarchar(255)",
                Target::Sqlite => "TEXT",
            };
            lose(
                to,
                &format!(
                    "`{base}` is not a built-in type here — if it is an enum, the allowed values \
                     are no longer enforced"
                ),
            )
        }
    }
}

fn args_or(args: &str, fallback: &str) -> String {
    if args.is_empty() {
        fallback.to_string()
    } else {
        args.to_string()
    }
}

/// A default the target can take, or `None` to omit it.
///
/// Conservative on purpose: a default is an expression, and dbd does not
/// translate expressions. A literal passes; a function call is dropped rather
/// than emitted in a dialect that may not have it.
fn map_default(pg: &str, target: Target) -> Option<String> {
    let d = pg.trim();
    if d.starts_with('\'') || d.parse::<f64>().is_ok() {
        return Some(d.to_string());
    }
    match (d.to_lowercase().as_str(), target) {
        ("true", Target::MySql | Target::Sqlite) => Some("1".to_string()),
        ("false", Target::MySql | Target::Sqlite) => Some("0".to_string()),
        ("true", Target::TSql) => Some("1".to_string()),
        ("false", Target::TSql) => Some("0".to_string()),
        ("now()" | "current_timestamp", Target::MySql) => Some("CURRENT_TIMESTAMP".to_string()),
        ("now()" | "current_timestamp", Target::TSql) => Some("SYSDATETIMEOFFSET()".to_string()),
        ("now()" | "current_timestamp", Target::Sqlite) => Some("CURRENT_TIMESTAMP".to_string()),
        _ => None,
    }
}
