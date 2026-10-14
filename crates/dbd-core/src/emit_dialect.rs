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
//! # Keys, checks and indexes
//!
//! Every target has FOREIGN KEY, CHECK, CREATE INDEX and stored generated
//! columns, so they are carried. A CHECK expression, a generated column's
//! expression or an index predicate is PostgreSQL SQL that dbd does not
//! translate, so it goes across verbatim and is reported, like a view body —
//! except a CHECK and a view body on SQLite, which are written again from the
//! parse tree or left out (next section). An
//! index the target would reject outright — an expression key on SQL Server, a
//! key on an unbounded text column on MySQL or SQL Server — is left out and
//! reported, so the script still applies. So is a foreign key to a table the
//! script does not create — one declared `external:`, or left out by `--scope`.
//!
//! A *constraint* on unbounded text — a primary key, UNIQUE, a foreign key —
//! is not left out: MySQL and SQL Server refuse it as it is, and dropping it
//! would lose the key and every foreign key that points at it. The column is
//! bounded instead (VARCHAR(255), nvarchar(450)) and the bound is reported.
//!
//! # Lengths, precisions and row sizes
//!
//! A length or precision is carried only as far as the target's documented
//! limit. Past it the target refuses the DDL (`nvarchar(5000)`,
//! `DECIMAL(50,40)`, `VARCHAR(70000)`), so the column becomes the target's
//! larger type — its long text, or its widest decimal, keeping the digits
//! before the point first — and the change is reported. A type with no limit
//! in PostgreSQL (`text`, an unbounded `varchar`, `bytea`) gets one with none
//! on the target (LONGTEXT, nvarchar(max), TEXT), which is faithful and not
//! reported; an unbounded `numeric` gets the widest decimal, which is not, and
//! is. A table whose columns are each legal can still be refused for its row:
//! MySQL's 65,535 bytes outside TEXT and BLOB, SQL Server's 8,060 of
//! fixed-length columns. Its widest columns are moved to a type that does not
//! count — TEXT, nvarchar — until it fits, and each is reported. Each limit is
//! cited where it is defined.
//!
//! # View bodies and CHECKs on SQLite
//!
//! Copied verbatim, they failed the script: a view naming `core.cities` (the
//! table is `core_cities` there), a CHECK using `POSITION(… IN …)` or `~`. So on
//! SQLite each is written again from libpg_query's tree — table names resolved
//! the way the file's `search_path` resolves them and flattened, functions and
//! operators mapped — or, at the first construct SQLite has no form for, left
//! out and reported, so the script applies. A view reading a table the script
//! does not create (an `external:` one, one outside the scope, a view left out)
//! is left out the same way. What is written is still reported: the SQL is
//! SQLite's, and so are its semantics. `emit_dialect/sqlite.rs` says why both,
//! and not one or the other.
//!
//! # Materialized views
//!
//! None of the three has them. A materialized view becomes a plain view — the
//! same rows, computed on every read — and that is reported, as is each of its
//! indexes, which a plain view cannot take and which are left out.
//!
//! # Identity columns
//!
//! An identity column — or `serial`, the same thing under an older name — is
//! numbered by the target: MySQL `AUTO_INCREMENT`, SQL Server `IDENTITY(1,1)`,
//! SQLite `INTEGER PRIMARY KEY AUTOINCREMENT`. Where the target's numbering
//! takes or refuses an explicit value differently from ALWAYS / BY DEFAULT, or
//! cannot number the column at all, it is reported.
//!
//! # Sequences
//!
//! SQL Server has `CREATE SEQUENCE`, so a sequence is emitted there with every
//! bound resolved to PostgreSQL's and spelled out — SQL Server's own defaults
//! start at the type's minimum — and `nextval('s')` becomes `NEXT VALUE FOR`.
//! MySQL and SQLite have none: the sequence is reported, an integer key that
//! drew from it is numbered by the table instead (reported, as the nearest
//! thing), and any other column loses the default (reported).
//!
//! # Defaults
//!
//! A default is read from libpg_query's tree. A literal goes across without
//! PostgreSQL's cast; the clock and a random UUID become the target's own.
//! Anything else is PostgreSQL SQL that dbd does not translate, so it is left
//! out and reported — a row that omits the column gets NULL, or is refused.
//!
//! # Comments
//!
//! MySQL keeps a table's and a column's (`COMMENT`), SQL Server a table's, a
//! column's and a view's (the `MS_Description` extended property SSMS shows).
//! Every other comment — all of them on SQLite, a view's on MySQL — is
//! reported, since it was documentation someone wrote.
//!
//! # SQL Server's schemas and batches
//!
//! SQL Server keeps the schema in a name, so the script creates every schema
//! it names before using it. `CREATE SCHEMA` and `CREATE VIEW` must each be
//! alone in a batch, so the T-SQL script ends every statement with `GO`.
//!
//! # What is not emitted
//!
//! Functions, procedures and triggers. Their bodies are PL/pgSQL or SQL that
//! does not translate, and dbd holds them as opaque text — emitting them would
//! produce something that looks convertible and is not. They are reported as
//! skipped so the omission is visible — a trigger written beside its table too,
//! which the model keeps only as an edge and is read back from the file. So is
//! anything whose file does not parse: there is no structure to translate, and
//! the schema is missing it.
//!
//! Extensions, roles, grants and RLS policies are not emitted either: none of
//! the three loads PostgreSQL extensions, SQLite has no roles, permissions or
//! row-level security at all, and the others model them differently enough
//! that a translation would look right and grant the wrong thing. Each is
//! reported — one entry per extension and role, per schema's grants, and per
//! policy file — narrowed to the scope like everything else, so a script whose
//! security rested on a policy does not read as complete.

use crate::entity::{
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IdentityKind, IndexDef, IndexType, SortOrder, TableConstraint,
    TableDef,
};
use crate::error::{DbdError, Result};
use crate::parser::Dialect;
use std::collections::HashSet;

mod sqlite;

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
    ///
    /// One line, always: a note quotes what was lost — a comment, an
    /// expression — and that can span lines. A second line would not be a
    /// comment, and the script would break on it.
    fn comment(&self, target: Target) -> String {
        let what = match &self.column {
            Some(c) => format!("`{c}` was {}", self.from),
            None => format!("{} was {}", self.entity, self.from),
        };
        let note = format!("{what} — emitted as {}; {}", self.to, self.reason);
        format!(
            "{} dbd: {}",
            target.comment_prefix(),
            note.split_whitespace().collect::<Vec<_>>().join(" ")
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

    /// Quote an identifier the way the target does, its closing quote
    /// doubled inside the name — `` ` `` on MySQL, `]` on SQL Server, `"` on
    /// SQLite. Wrapped as it was, a name holding one ended early: SQLite got
    /// `"app_we"ird"`, and the script did not run.
    fn quote(self, name: &str) -> String {
        match self {
            Self::MySql => format!("`{}`", name.replace('`', "``")),
            Self::TSql => format!("[{}]", name.replace(']', "]]")),
            Self::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
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
    let entities = match scope {
        Some(s) => design.scoped_entities(s)?,
        None => design.entities().to_vec(),
    };
    emit_sections(&[(None, entities)], design.entities(), &[(design, scope)], target)
}

/// [`emit_schema`] over what a command reads (#7): a project's design, or a
/// workspace's modules as one schema — so a key into a dependency's table is
/// kept, the script creating that table too — each module's statements after
/// a comment naming it.
pub fn emit_reading(reading: &crate::design::Reading, target: Dialect) -> Result<(String, Vec<Downgrade>)> {
    let sections: Vec<(Option<String>, Vec<Entity>)> = reading
        .sections_in_view()?
        .into_iter()
        .map(|(module, entities)| {
            let label = module.name().map(|name| format!("module {name} ({})", module.owner()));
            (label, entities)
        })
        .collect();
    let designs: Vec<_> = reading.parts().map(|(design, scope)| (design, Some(scope))).collect();
    emit_sections(&sections, &reading.declared(), &designs, target)
}

/// The script for `sections` — each its statements, after a comment with its
/// label when it has one — drawn against `declared`, every entity of
/// `designs`, which each answer for their grants and policies.
fn emit_sections(
    sections: &[(Option<String>, Vec<Entity>)],
    declared: &[Entity],
    designs: &[(&crate::Design, Option<&crate::scope::ResolvedScope>)],
    target: Dialect,
) -> Result<(String, Vec<Downgrade>)> {
    let target = Target::from_dialect(target)?;
    let design = designs[0].0;
    if !design.parser().produces_structure() {
        return Err(DbdError::Config(format!(
            "emit needs a structured model and this project has none (source.dialect: {}) — \
             its DDL is read for identity and references, not columns, so there is nothing \
             to translate. Only a PostgreSQL project can be emitted as another dialect.",
            design.dialect()
        )));
    }

    let entities: Vec<&Entity> = sections.iter().flat_map(|(_, section)| section).collect();

    let mut header = format!(
        "{} Generated by dbd emit --dialect {} — from a PostgreSQL project.\n\
         {} Downgrades are noted inline; see the run's report for the full list.",
        target.comment_prefix(),
        match target {
            Target::MySql => "mysql",
            Target::TSql => "tsql",
            Target::Sqlite => "sqlite",
        },
        target.comment_prefix(),
    );
    if target == Target::TSql {
        header.push_str(
            "\n-- Batches are separated by GO, as sqlcmd and SSMS expect; a driver that sends \
             the script in one call must split on it first.",
        );
    }
    let script = Script {
        target,
        tables: entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Table && e.errors.is_empty() && e.table_def.is_some())
            .map(|e| e.name.clone())
            .collect(),
        externals: declared
            .iter()
            .filter(|e| e.entity_type == EntityType::External)
            .map(|e| e.name.clone())
            .collect(),
        relations: declared
            .iter()
            .filter(|e| {
                matches!(
                    e.entity_type,
                    EntityType::Table | EntityType::View | EntityType::MaterializedView | EntityType::External
                )
            })
            .map(|e| e.name.clone())
            .collect(),
        sequences: entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Sequence && e.errors.is_empty())
            .map(|e| e.name.clone())
            .collect(),
        enums: declared
            .iter()
            .filter(|e| e.entity_type == EntityType::Enum)
            .map(|e| e.name.clone())
            .collect(),
    };
    let mut out: Vec<String> = Vec::new();
    let mut report = Vec::new();
    // Every view the script has created so far — in dependency order, so a
    // view reading one left out finds it missing.
    let mut views: HashSet<String> = HashSet::new();

    // SQL Server keeps the schema in every name it is given (`[app].[orders]`),
    // and a schema that does not exist fails the first statement that names
    // it. `dbo` is there in every database. Derived from what is emitted rather
    // than from the design's schema list, so a scope that keeps a schema's
    // tables but not the schema entity still gets the schema it needs.
    if target.has_schemas() {
        let mut schemas: Vec<&str> = Vec::new();
        for e in entities.iter().filter(|e| e.errors.is_empty() && emits_an_object(e)) {
            if let Some(s) = e.schema.as_deref()
                && s != "dbo"
                && !schemas.contains(&s)
            {
                schemas.push(s);
            }
        }
        out.extend(schemas.iter().map(|s| format!("CREATE SCHEMA {};", target.quote(s))));
    }

    for (label, section) in sections {
        if let Some(label) = label {
            out.push(format!("{} {label}", target.comment_prefix()));
        }
        for e in section {
            // A file that does not parse has no structure to translate, so it
            // cannot be emitted — but leaving it out without a word gave a schema
            // missing a table, an exit of 0 and an empty report.
            if !e.errors.is_empty() {
                if !matches!(
                    e.entity_type,
                    EntityType::External
                        | EntityType::Import
                        | EntityType::Schema
                        | EntityType::Extension
                        | EntityType::Role
                ) {
                    let d = Downgrade {
                        entity: e.name.clone(),
                        column: None,
                        from: format!("a {} whose file could not be parsed", e.entity_type.tag()),
                        to: "nothing".to_string(),
                        reason: format!(
                            "{} — the emitted schema is missing it until the file is fixed",
                            e.errors.join("; ")
                        ),
                    };
                    out.push(d.comment(target));
                    report.push(d);
                }
                continue;
            }
            match e.entity_type {
                EntityType::Table => {
                    out.push(emit_table(e, &script, &mut report));
                    out.extend(emit_indexes(e, &script, &mut report));
                    out.extend(descriptions(e, target));
                    out.extend(skipped_triggers(e, target, &mut report));
                }
                EntityType::View | EntityType::MaterializedView => {
                    if let Some(sql) = emit_view(e, &script, &mut views, &mut report) {
                        out.push(sql);
                        out.extend(descriptions(e, target));
                    }
                }
                EntityType::Sequence => {
                    if let Some(sql) = emit_sequence(e, target, &mut report) {
                        out.push(sql);
                    }
                }
                EntityType::Function | EntityType::Procedure | EntityType::Trigger => {
                    // Noted in the file as well as listed: a reader of the script
                    // alone otherwise has no sign the routine ever existed.
                    let d = Downgrade {
                        entity: e.name.clone(),
                        column: None,
                        from: format!("a {}", e.entity_type.tag()),
                        to: "nothing".to_string(),
                        reason: UNTRANSLATED_ROUTINE.to_string(),
                    };
                    out.push(d.comment(target));
                    report.push(d);
                }
                EntityType::Extension | EntityType::Role => {
                    let d = not_carried(e, target);
                    out.push(d.comment(target));
                    report.push(d);
                }
                // A schema becomes `CREATE SCHEMA` on SQL Server and part of every
                // name elsewhere (reported at each table); an enum becomes its
                // columns' type. Neither is a statement of its own.
                _ => {}
            }
        }
    }

    // Last, where they would apply: after every object they name.
    for (design, scope) in designs {
        for d in grants_not_carried(design, *scope, target)?
            .into_iter()
            .chain(policies_not_carried(design, *scope, target)?)
        {
            out.push(d.comment(target));
            report.push(d);
        }
    }

    Ok((join_script(&header, &out, target), report))
}

/// An extension or a role, which the script does not create.
///
/// Neither was mentioned anywhere — not in the file, not in the report — so a
/// schema that needed `pgcrypto`, or a login its grants were written for, came
/// out looking complete.
fn not_carried(e: &Entity, target: Target) -> Downgrade {
    let label = target.label();
    let (from, reason) = match (e.entity_type, target) {
        (EntityType::Extension, _) => (
            "a PostgreSQL extension",
            format!(
                "{label} cannot load PostgreSQL extensions — whatever the schema took from it (a \
                 function, a type, an operator) has to be replaced by hand"
            ),
        ),
        (_, Target::Sqlite) => (
            "a role",
            "SQLite has no roles or permissions — anything that can open the file can read and \
             write all of it"
                .to_string(),
        ),
        (_, Target::MySql | Target::TSql) => (
            "a role",
            format!(
                "a PostgreSQL role is a login and a group at once, with attributes {label} models \
                 differently, so dbd does not translate it — create it by hand"
            ),
        ),
    };
    Downgrade {
        entity: e.name.clone(),
        column: None,
        from: from.to_string(),
        to: "nothing".to_string(),
        reason,
    }
}

/// One entry per schema whose grants a run under `scope` would apply — the
/// same set `dbd apply` grants ([`crate::Design::declared_grants`]).
fn grants_not_carried(
    design: &crate::Design,
    scope: Option<&crate::scope::ResolvedScope>,
    target: Target,
) -> Result<Vec<Downgrade>> {
    let mut grants: Vec<(String, std::collections::HashMap<String, Vec<String>>)> =
        design.declared_grants(scope)?.into_iter().collect();
    grants.sort_by(|a, b| a.0.cmp(&b.0));
    let reason = match target {
        Target::Sqlite => {
            "SQLite has no permissions — anything that can open the file can read and write all of it".to_string()
        }
        Target::MySql | Target::TSql => format!(
            "dbd does not translate grants, and the roles they name are not emitted either — on {}, \
             grant the same access by hand once the roles exist",
            target.label()
        ),
    };
    Ok(grants
        .into_iter()
        .map(|(schema, roles)| {
            let mut roles: Vec<(String, Vec<String>)> = roles.into_iter().collect();
            roles.sort();
            let held: Vec<String> = roles
                .iter()
                .map(|(role, privileges)| format!("{role}: {}", privileges.join(", ")))
                .collect();
            Downgrade {
                from: format!("grants on schema `{schema}` ({})", held.join("; ")),
                entity: schema,
                column: None,
                to: "nothing".to_string(),
                reason: reason.clone(),
            }
        })
        .collect())
}

/// One entry per RLS policy file a run under `scope` would apply — the same
/// filter `dbd policies` runs ([`crate::design::plan_policies`]).
///
/// A policy is what decides which rows a reader may see. Dropping one without
/// a word left a script that applies cleanly and shows every row to anyone.
fn policies_not_carried(
    design: &crate::Design,
    scope: Option<&crate::scope::ResolvedScope>,
    target: Target,
) -> Result<Vec<Downgrade>> {
    let project_dir = design.project_dir();
    let working_set = match scope.filter(|s| !s.is_all) {
        Some(s) => Some((s.name.as_str(), design.working_set(s)?)),
        None => None,
    };
    let plan = crate::design::plan_policies(project_dir, working_set.as_ref().map(|(n, ws)| (*n, ws)))?;
    let reason = match target {
        Target::MySql | Target::Sqlite => format!(
            "{} has no row-level security, so every row is visible to anyone who can read the table",
            target.label()
        ),
        Target::TSql => "SQL Server's row-level security is a security policy over an inline function, not \
                         something these statements translate to — until one is written, every row is visible \
                         to anyone who can read the table"
            .to_string(),
    };
    Ok(plan
        .applied
        .iter()
        .map(|file| {
            let shown = file.strip_prefix(project_dir).unwrap_or(file).display().to_string();
            Downgrade {
                // A file outside the `policies/<schema>/<table>.sql` layout
                // names no table; the file is then the only identity it has.
                entity: crate::design::policy_target(file, project_dir).unwrap_or_else(|| shown.clone()),
                column: None,
                from: format!("the row-level security policies in `{shown}`"),
                to: "nothing".to_string(),
                reason: reason.clone(),
            }
        })
        .collect())
}

/// Why a routine — and a trigger, which only fires one — is not emitted.
///
/// One sentence for all of them: a trigger is skipped because of what it runs,
/// and two entries saying the same thing in different words make a reader
/// wonder whether they follow different rules.
const UNTRANSLATED_ROUTINE: &str = "its body is PL/pgSQL or SQL that does not translate, and dbd holds it as \
                                    opaque text — emitting it would look convertible and not be";

/// The triggers declared in `e`'s table file, each reported as skipped and
/// noted where the table is.
///
/// A trigger beside its table is not an entity of its own — the model keeps it
/// only as an edge to the routine it fires — so leaving it out reported
/// nothing at all. It is read back from the file, as a sequence's options are.
fn skipped_triggers(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> Vec<String> {
    let names = e
        .file
        .as_deref()
        .and_then(|f| crate::source_text::read_to_string(f).ok())
        .map(|sql| crate::parser::pg::tables::trigger_names(&sql))
        .unwrap_or_default();
    names
        .into_iter()
        .map(|name| {
            let d = Downgrade {
                entity: e.name.clone(),
                column: None,
                from: format!("the trigger `{name}`"),
                to: "nothing".to_string(),
                reason: UNTRANSLATED_ROUTINE.to_string(),
            };
            let note = d.comment(target);
            report.push(d);
            note
        })
        .collect()
}

/// What the script as a whole holds. A statement naming something outside it —
/// a key to a table it does not create — cannot apply on its own.
struct Script {
    target: Target,
    /// `schema.name` of every table the script creates.
    tables: HashSet<String>,
    /// `schema.name` of every table the design declares `external:`.
    externals: HashSet<String>,
    /// `schema.name` of every table, view, materialized view and external
    /// table in the whole design — what a bare name in a view body can
    /// resolve to, whether or not the script creates it.
    relations: HashSet<String>,
    /// `schema.name` of every sequence in the emitted set. Only SQL Server
    /// creates them; a default drawing from one outside it cannot apply.
    sequences: HashSet<String>,
    /// `schema.name` of every enum the design declares — the whole design,
    /// since a column's type is what it is whatever the scope keeps.
    enums: HashSet<String>,
}

impl Script {
    /// Whether the type a column names is one of this project's enums. A bare
    /// name matches an enum of that name in any schema: the column's file may
    /// have reached it through its `search_path`.
    fn is_enum(&self, ty: &str) -> bool {
        self.enums.contains(ty) || (!ty.contains('.') && self.enums.iter().any(|n| n.rsplit('.').next() == Some(ty)))
    }

    /// What a table name in `e`'s body is in the script: `schema.name` as
    /// written, or a bare `name` found the way PostgreSQL finds it — the first
    /// schema on the file's `search_path` that has one, then `public`, which
    /// dbd puts on every applied path. `views` is every view created so far.
    fn relation(&self, e: &Entity, schema: Option<&str>, name: &str, views: &HashSet<String>) -> sqlite::Relation {
        let found = match schema {
            Some(s) => format!("{s}.{name}"),
            None => e
                .schema_path
                .schemas()
                .chain(std::iter::once(crate::reconcile::DEFAULT_SCHEMA))
                .map(|s| format!("{s}.{name}"))
                .find(|q| self.relations.contains(q))
                .unwrap_or_else(|| name.to_string()),
        };
        if self.tables.contains(&found) || views.contains(&found) {
            let (schema, bare) = match found.split_once('.') {
                Some((s, b)) => (Some(s), b),
                None => (None, found.as_str()),
            };
            return sqlite::Relation::Created(qualified(schema, bare, self.target));
        }
        let why = if self.externals.contains(&found) {
            "it is declared `external:`, someone else's to create"
        } else if self.relations.contains(&found) {
            "it is outside the scope, or was itself left out"
        } else {
            "nothing in the project declares it"
        };
        sqlite::Relation::Missing(format!("it reads `{found}`, which the script does not create — {why}"))
    }
}

/// Whether `e` becomes an object in the target — and so needs its schema to
/// exist first.
fn emits_an_object(e: &Entity) -> bool {
    matches!(
        e.entity_type,
        EntityType::Table | EntityType::View | EntityType::MaterializedView | EntityType::Sequence
    )
}

/// The header, then every statement.
///
/// SQL Server requires `CREATE SCHEMA` and `CREATE VIEW` to be the only
/// statement in their batch, so its script ends every statement with `GO` —
/// the batch separator sqlcmd and SSMS read. Not T-SQL itself, which is why
/// the header says so; MySQL and SQLite would take it for a statement.
fn join_script(header: &str, statements: &[String], target: Target) -> String {
    let (sep, end) = match target {
        Target::TSql => ("\nGO\n\n", "\nGO\n"),
        Target::MySql | Target::Sqlite => ("\n\n", "\n"),
    };
    if statements.is_empty() {
        return format!("{header}\n");
    }
    format!("{header}\n\n{}{end}", statements.join(sep))
}

/// `schema.name` in the target's form, without reporting: the flattening of a
/// table is reported once, at the table itself, not at every reference to it.
fn qualified(schema: Option<&str>, bare: &str, target: Target) -> String {
    match (schema, target.has_schemas()) {
        (Some(s), true) => format!("{}.{}", target.quote(s), target.quote(bare)),
        (Some(s), false) => target.quote(&format!("{s}_{bare}")),
        (None, _) => target.quote(bare),
    }
}

/// `schema.name` flattened for a target without schemas.
fn table_name(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> String {
    let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
    match (&e.schema, target.has_schemas()) {
        (Some(_), true) | (None, _) => qualified(e.schema.as_deref(), bare, target),
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
            qualified(Some(s), bare, target)
        }
    }
}

/// Which key [`key_name`] names.
enum Key<'a> {
    /// The primary key, on these columns.
    Primary(&'a [String]),
    Unique,
}

/// `CONSTRAINT <name> ` for a key the design names, where the target keeps the
/// name — or nothing. Every target takes the clause on a UNIQUE, and SQL
/// Server and SQLite on a primary key; MySQL names every primary key
/// `PRIMARY`, whatever the DDL calls it, so there the name is reported (and
/// noted above the key, in `lines`) rather than written to be ignored.
fn key_name(
    e: &Entity,
    name: Option<&str>,
    key: Key,
    target: Target,
    report: &mut Vec<Downgrade>,
    lines: &mut Vec<String>,
) -> String {
    let Some(name) = name else {
        return String::new();
    };
    match key {
        Key::Primary(columns) if target == Target::MySql => {
            let d = Downgrade {
                entity: e.name.clone(),
                column: None,
                from: format!("the primary key's name `{name}` ({})", columns.join(", ")),
                to: "the name `PRIMARY`".to_string(),
                reason: "MySQL names every primary key PRIMARY, whatever the DDL calls it".to_string(),
            };
            lines.push(format!("  {}", d.comment(target)));
            report.push(d);
            String::new()
        }
        _ => format!("CONSTRAINT {} ", target.quote(name)),
    }
}

fn emit_table(e: &Entity, script: &Script, report: &mut Vec<Downgrade>) -> String {
    let target = script.target;
    let Some(td) = &e.table_def else {
        return format!("{} dbd: {} has no readable structure.", target.comment_prefix(), e.name);
    };
    let name = table_name(e, target, report);

    // A PARSED table carries its key on the column (`is_pk`/`is_unique`); only
    // the reconcile path lifts those into constraints. Reading constraints
    // alone emitted a table with no key — valid DDL, wrong schema, and a loss
    // the report could not have caught because nothing knew it happened.
    let inline_pk: Vec<String> = td.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
    let pk: Vec<String> = if inline_pk.is_empty() {
        td.constraints
            .iter()
            .find_map(|con| match con {
                TableConstraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                _ => None,
            })
            .unwrap_or_default()
    } else {
        inline_pk.clone()
    };
    // The primary key's name, when the design gives it one.
    let pk_name = td.constraints.iter().find_map(|con| match con {
        TableConstraint::PrimaryKey { name: Some(name), .. } => Some(name.as_str()),
        _ => None,
    });
    let numbered = numbered_column(td, &pk, target, e.schema.as_deref());
    let types = column_types(e, td, script);

    let mut lines: Vec<String> = Vec::new();
    for (c, typed) in td.columns.iter().zip(types) {
        let before = report.len();
        report.extend(typed.notes);
        let ty = typed.ty;
        let numbering = numbering(e, c, numbered, target, report);
        let default = c
            .default_value
            .as_deref()
            .and_then(|d| column_default(d, e, c, &ty, numbered, script, report));
        // All three compute a stored column, so a generated one stays
        // generated — emitting it plain left a column holding whatever an
        // insert put there. Its expression is PostgreSQL SQL dbd does not
        // translate, so it goes across verbatim and is reported, like a CHECK.
        if let Some(expr) = &c.generated {
            report.push(Downgrade {
                entity: e.name.clone(),
                column: Some(c.name.clone()),
                from: format!("a generated column's expression in PostgreSQL SQL (`{expr}`)"),
                to: "the same text, untranslated".to_string(),
                reason: "dbd translates types and structure, not expressions — anything \
                         PostgreSQL-specific inside it has to be checked by hand"
                    .to_string(),
            });
        }
        let comment = column_comment(td, c);
        if let Some(text) = comment
            && target == Target::Sqlite
        {
            report.push(comment_lost(e, Some(&c.name), text, target));
        }
        // A note goes immediately above the column it explains.
        for d in &report[before..] {
            lines.push(format!("  {}", d.comment(target)));
        }
        let mut col = match (&c.generated, target) {
            // SQL Server's computed column takes its type from the expression;
            // the CAST keeps the declared one.
            (Some(expr), Target::TSql) => format!("  {} AS CAST(({expr}) AS {ty}) PERSISTED", target.quote(&c.name)),
            (Some(expr), Target::MySql | Target::Sqlite) => {
                format!("  {} {ty} GENERATED ALWAYS AS ({expr}) STORED", target.quote(&c.name))
            }
            (None, _) => format!("  {} {ty}", target.quote(&c.name)),
        };
        // `[id] bigint IDENTITY(1,1) NOT NULL`, `"id" INTEGER PRIMARY KEY
        // AUTOINCREMENT NOT NULL`, `` `id` INT NOT NULL AUTO_INCREMENT `` —
        // each engine's own documented order.
        if let Some(clause) = numbering
            && target != Target::MySql
        {
            // SQLite's numbered column is the primary key itself
            // (`PRIMARY KEY AUTOINCREMENT`), so the key's name goes here.
            let named = match pk_name {
                Some(name) if target == Target::Sqlite => format!("CONSTRAINT {} ", target.quote(name)),
                _ => String::new(),
            };
            col.push_str(&format!(" {named}{clause}"));
        }
        if !c.nullable {
            col.push_str(" NOT NULL");
        }
        if let Some(clause) = numbering
            && target == Target::MySql
        {
            col.push_str(&format!(" {clause}"));
        }
        if let Some(default) = default {
            col.push_str(&format!(" DEFAULT {default}"));
        }
        if let Some(text) = comment
            && target == Target::MySql
        {
            col.push_str(&format!(" COMMENT {}", string_literal(text, target)));
        }
        lines.push(format!("{col},"));
    }

    // SQLite's numbered column declares the key in place; a second, table-level
    // PRIMARY KEY is refused.
    let key_in_place = target == Target::Sqlite && numbered.is_some();
    // One PRIMARY KEY: a table with two is rejected by every target, and a
    // table-level key marks its columns `is_pk` as well.
    if !pk.is_empty() && !key_in_place {
        let named = key_name(e, pk_name, Key::Primary(&pk), target, report, &mut lines);
        lines.push(format!("  {named}PRIMARY KEY ({}),", quote_all(&pk, target)));
    }
    for c in td.columns.iter().filter(|c| c.is_unique && !c.is_pk) {
        lines.push(format!("  UNIQUE ({}),", target.quote(&c.name)));
    }
    for con in &td.constraints {
        match con {
            TableConstraint::Unique {
                name,
                columns,
                nulls_not_distinct,
            } if !columns.is_empty() => {
                // SQL Server's UNIQUE already lets only one NULL in, as `NULLS NOT
                // DISTINCT` does; the other two treat NULLs as distinct.
                if *nulls_not_distinct && target != Target::TSql {
                    let d = Downgrade {
                        entity: e.name.clone(),
                        column: None,
                        from: format!("`NULLS NOT DISTINCT` on the UNIQUE ({})", columns.join(", ")),
                        to: "NULLs that do not collide".to_string(),
                        reason: format!(
                            "{}'s UNIQUE treats NULLs as distinct, so the table now admits rows with NULL keys \
                         that it refused",
                            target.label()
                        ),
                    };
                    lines.push(format!("  {}", d.comment(target)));
                    report.push(d);
                }
                let named = key_name(e, name.as_deref(), Key::Unique, target, report, &mut lines);
                lines.push(format!("  {named}UNIQUE ({}),", quote_all(columns, target)));
            }
            // No other engine has an exclusion constraint, and no other kind
            // of constraint enforces what it does.
            TableConstraint::Exclude { name, definition } => {
                let d = Downgrade {
                    entity: e.name.clone(),
                    column: None,
                    from: format!(
                        "an exclusion constraint{} (`EXCLUDE {definition}`)",
                        name.as_deref().map(|n| format!(" `{n}`")).unwrap_or_default()
                    ),
                    to: "no constraint".to_string(),
                    reason: "the target has no EXCLUDE, so rows it refused — two that conflict under its \
                             operators — are now accepted"
                        .to_string(),
                };
                lines.push(format!("  {}", d.comment(target)));
                report.push(d);
            }
            _ => {}
        }
    }

    // Every target has FOREIGN KEY, so a key is carried: dropping one left a
    // schema that applied cleanly and enforced nothing.
    let inline = td.columns.iter().filter_map(|c| c.inline_fk.as_ref());
    let declared = td.constraints.iter().filter_map(|c| match c {
        TableConstraint::ForeignKey(fk) => Some(fk),
        _ => None,
    });
    for fk in inline.chain(declared) {
        let before = report.len();
        let clause = foreign_key(e, fk, script, report);
        for d in &report[before..] {
            lines.push(format!("  {}", d.comment(target)));
        }
        if let Some(clause) = clause {
            lines.push(format!("  {clause},"));
        }
    }

    // CHECK exists everywhere, but its expression is PostgreSQL SQL. SQLite
    // gets it written again from the parse tree, or not at all — a CHECK it
    // cannot parse fails the CREATE TABLE and takes the table with it (see
    // [`sqlite`]). The others get it verbatim, and reported.
    for con in &td.constraints {
        if let TableConstraint::Check { name, expression } = con {
            let named = name
                .as_ref()
                .map(|n| format!("CONSTRAINT {} ", target.quote(n)))
                .unwrap_or_default();
            if target == Target::Sqlite {
                let from = format!("a CHECK expression in PostgreSQL SQL (`{expression}`)");
                let (d, check) = match sqlite::check(expression) {
                    Ok(sql) => (
                        Downgrade {
                            entity: e.name.clone(),
                            column: None,
                            from,
                            to: "the same check in SQLite's SQL".to_string(),
                            reason: SQLITE_REWRITTEN.to_string(),
                        },
                        Some(sql),
                    ),
                    Err(sqlite::NoForm(why)) => (
                        Downgrade {
                            entity: e.name.clone(),
                            column: None,
                            from,
                            to: "no CHECK".to_string(),
                            reason: format!(
                                "{why} — so the CHECK is left out and the table is still created; rows it \
                                 refused are now accepted"
                            ),
                        },
                        None,
                    ),
                };
                lines.push(format!("  {}", d.comment(target)));
                report.push(d);
                if let Some(sql) = check {
                    lines.push(format!("  {named}CHECK ({sql}),"));
                }
                continue;
            }
            let d = Downgrade {
                entity: e.name.clone(),
                column: None,
                from: format!("a CHECK expression in PostgreSQL SQL (`{expression}`)"),
                to: "the same text, untranslated".to_string(),
                reason: "dbd translates types and structure, not expressions — anything \
                         PostgreSQL-specific inside it has to be checked by hand"
                    .to_string(),
            };
            lines.push(format!("  {}", d.comment(target)));
            report.push(d);
            lines.push(format!("  {named}CHECK ({expression}),"));
        }
    }

    // MySQL keeps a table's comment as a table option; SQL Server as an
    // extended property, written after the table ([`descriptions`]); SQLite
    // keeps none.
    let mut head = String::new();
    let mut options = String::new();
    match (td.comments.table.as_deref(), target) {
        (Some(text), Target::MySql) => options = format!(" COMMENT={}", string_literal(text, target)),
        (Some(text), Target::Sqlite) => {
            let d = comment_lost(e, None, text, target);
            head = format!("{}\n", d.comment(target));
            report.push(d);
        }
        _ => {}
    }
    format!("{head}CREATE TABLE {name} (\n{}\n){options};", table_body(&lines))
}

/// Column `c`'s comment — on the column, or recorded with the table's.
fn column_comment<'a>(td: &'a TableDef, c: &'a ColumnDef) -> Option<&'a str> {
    c.comment
        .as_deref()
        .or_else(|| td.comments.columns.get(&c.name).map(String::as_str))
}

/// A comment the target has nowhere to keep — documentation someone wrote,
/// gone from the emitted schema.
fn comment_lost(e: &Entity, column: Option<&str>, text: &str, target: Target) -> Downgrade {
    let on = match (column, e.entity_type) {
        (Some(_), _) => "a column",
        (None, EntityType::View | EntityType::MaterializedView) => "a view",
        (None, _) => "a table",
    };
    Downgrade {
        entity: e.name.clone(),
        column: column.map(str::to_string),
        from: format!("the comment `{text}`"),
        to: "nothing".to_string(),
        reason: format!("{} keeps no comment on {on}", target.label()),
    }
}

/// SQL Server's comments: an `MS_Description` extended property — the one SSMS
/// shows — per commented table, column and view, in a batch after the object.
/// A view's must be: `CREATE VIEW` is alone in its batch.
fn descriptions(e: &Entity, target: Target) -> Option<String> {
    if target != Target::TSql {
        return None;
    }
    let n = |s: &str| format!("N'{}'", s.replace('\'', "''"));
    let schema = e.schema.as_deref().unwrap_or("dbo");
    let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
    let (level1, items): (&str, Vec<(Option<&str>, &str)>) = match (e.entity_type, &e.table_def) {
        (EntityType::Table, Some(td)) => (
            "TABLE",
            td.comments
                .table
                .as_deref()
                .map(|t| (None, t))
                .into_iter()
                .chain(
                    td.columns
                        .iter()
                        .filter_map(|c| column_comment(td, c).map(|t| (Some(c.name.as_str()), t))),
                )
                .collect(),
        ),
        (EntityType::View | EntityType::MaterializedView, _) => {
            ("VIEW", e.comment.as_deref().map(|t| (None, t)).into_iter().collect())
        }
        _ => return None,
    };
    if items.is_empty() {
        return None;
    }
    let lines: Vec<String> = items
        .iter()
        .map(|(column, text)| {
            let mut sql = format!(
                "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = {}, \
                 @level0type = N'SCHEMA', @level0name = {}, @level1type = N'{level1}', @level1name = {}",
                n(text),
                n(schema),
                n(bare)
            );
            if let Some(c) = column {
                sql.push_str(&format!(", @level2type = N'COLUMN', @level2name = {}", n(c)));
            }
            sql + ";"
        })
        .collect();
    Some(lines.join("\n"))
}

/// The lines of a `CREATE TABLE`, without the comma after the last clause.
///
/// The last line is not always a clause: a note about something left out (a
/// key to a table the script does not create) can come after it, and a comma
/// left before the closing parenthesis is a syntax error on every target.
fn table_body(lines: &[String]) -> String {
    let last = lines.iter().rposition(|l| !l.trim_start().starts_with("--"));
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if Some(i) == last {
                l.trim_end_matches(',')
            } else {
                l.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where a column's values come from, when PostgreSQL draws them from a
/// sequence.
#[derive(Debug, Clone, PartialEq)]
enum Drawn {
    /// `GENERATED … AS IDENTITY`.
    Identity(IdentityKind),
    /// `serial` and its siblings: an integer with a sequence of its own, which
    /// takes an explicit value the way BY DEFAULT does.
    Serial,
    /// `DEFAULT nextval('seq')` on an integer column — the sequence,
    /// schema-qualified.
    NextVal(String),
}

/// How column `c` draws its values from a sequence, if it does. `schema` is
/// the table's, which an unqualified sequence name resolves to.
fn drawn(c: &ColumnDef, schema: Option<&str>) -> Option<Drawn> {
    if let Some(kind) = c.identity {
        return Some(Drawn::Identity(kind));
    }
    let t = c.data_type.trim().to_lowercase();
    if matches!(
        t.as_str(),
        "serial" | "bigserial" | "smallserial" | "serial2" | "serial4" | "serial8"
    ) {
        return Some(Drawn::Serial);
    }
    let integer = matches!(
        t.as_str(),
        "integer" | "int" | "int4" | "bigint" | "int8" | "smallint" | "int2"
    );
    match c.default_value.as_deref().map(|d| read_default(d, &c.data_type)) {
        Some(PgDefault::NextVal(written)) if integer => Some(Drawn::NextVal(sequence_name(&written, schema))),
        _ => None,
    }
}

/// The sequence `nextval('…')` names, schema-qualified the way `search_path`
/// would have — the table's own schema for a bare name.
fn sequence_name(written: &str, schema: Option<&str>) -> String {
    let parts: Vec<String> = written
        .split('.')
        .map(|p| p.trim().trim_matches('"').to_string())
        .collect();
    match (parts.as_slice(), schema) {
        ([bare], Some(s)) => format!("{s}.{bare}"),
        ([bare], None) => bare.clone(),
        // `db.schema.name` names the same sequence as `schema.name`.
        (many, _) => many[many.len() - 2..].join("."),
    }
}

/// Whether `column` leads a key declared inside the `CREATE TABLE` — the
/// primary key or a UNIQUE.
///
/// Not an index: MySQL checks that an AUTO_INCREMENT column leads a key when
/// the table is created, and an index is a separate `CREATE INDEX` after it.
fn leads_a_key(td: &TableDef, pk: &[String], column: &str) -> bool {
    let leads = |cols: &[String]| cols.first().is_some_and(|c| c == column);
    leads(pk)
        || td.columns.iter().any(|c| c.name == column && c.is_unique)
        || td.constraints.iter().any(|con| match con {
            TableConstraint::Unique { columns, .. } => leads(columns),
            _ => false,
        })
}

/// The one column the target numbers itself, if any.
///
/// Every target numbers at most one column per table, and two of them only
/// some columns: MySQL one that leads the primary key or a UNIQUE (InnoDB keeps the counter in that
/// index), SQLite only a single-column INTEGER PRIMARY KEY, which is the
/// rowid. The first sequence-backed column that qualifies gets it; the rest
/// are reported where they are emitted.
///
/// A column that drew from a named sequence is a candidate only where the
/// target has no sequences: SQL Server keeps drawing from the sequence itself.
fn numbered_column<'a>(td: &'a TableDef, pk: &[String], target: Target, schema: Option<&str>) -> Option<&'a str> {
    td.columns
        .iter()
        .filter(|c| match drawn(c, schema) {
            Some(Drawn::NextVal(_)) => target != Target::TSql,
            Some(_) => true,
            None => false,
        })
        .find(|c| match target {
            Target::TSql => true,
            Target::Sqlite => pk.len() == 1 && pk[0] == c.name,
            Target::MySql => leads_a_key(td, pk, &c.name),
        })
        .map(|c| c.name.as_str())
}

/// The clause that has the target number column `c` — `AUTO_INCREMENT`,
/// `IDENTITY(1,1)`, `PRIMARY KEY AUTOINCREMENT` — or `None` when it is not
/// sequence-backed or the target cannot number it.
///
/// Reported when the numbering behaves differently: GENERATED ALWAYS refuses
/// an explicit value and BY DEFAULT (and `serial`) takes one. MySQL's and
/// SQLite's counters take one, so ALWAYS loses its guard there; SQL Server's
/// IDENTITY refuses one, so BY DEFAULT is what loses there. A column the target
/// cannot number at all is a plain column, and reported, since inserts must now
/// supply it.
///
/// A column that drew from a named sequence is numbered only where the target
/// has no sequences, as the nearest thing — always reported, since the table's
/// counter is not that sequence. Where it is not numbered, its default says
/// what happened ([`column_default`]).
fn numbering(
    e: &Entity,
    c: &ColumnDef,
    numbered: Option<&str>,
    target: Target,
    report: &mut Vec<Downgrade>,
) -> Option<&'static str> {
    let drawn = drawn(c, e.schema.as_deref())?;
    let is_numbered = numbered == Some(c.name.as_str());
    if matches!(drawn, Drawn::NextVal(_)) && (target == Target::TSql || !is_numbered) {
        return None;
    }
    let from = match &drawn {
        Drawn::Identity(IdentityKind::Always) => "`GENERATED ALWAYS AS IDENTITY`".to_string(),
        Drawn::Identity(IdentityKind::ByDefault) => "`GENERATED BY DEFAULT AS IDENTITY`".to_string(),
        Drawn::Serial => format!("`{}`, numbered by a sequence of its own", c.data_type),
        Drawn::NextVal(_) => format!("the default `{}`", c.default_value.as_deref().unwrap_or_default()),
    };
    let mut lose = |to: &str, reason: String| {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: Some(c.name.clone()),
            from: from.clone(),
            to: to.to_string(),
            reason,
        });
    };

    if !is_numbered {
        let reason = match (numbered, target) {
            (Some(other), _) => format!(
                "{} numbers one column per table and `{other}` has it — inserts must now supply this one",
                target.label()
            ),
            (None, Target::MySql) => {
                "MySQL numbers only a column that leads the primary key or a UNIQUE, which this one does \
                 not — inserts must now supply it"
                    .to_string()
            }
            (None, Target::Sqlite) => {
                "SQLite numbers only a single-column INTEGER PRIMARY KEY, which this is not — inserts \
                 must now supply it"
                    .to_string()
            }
            // SQL Server numbers the first sequence-backed column of any table,
            // so this is `numbered` disagreeing with itself; say what is lost
            // rather than assert it cannot happen.
            (None, Target::TSql) => {
                "SQL Server was not given an IDENTITY for it — inserts must now supply it".to_string()
            }
        };
        lose("a plain column", reason);
        return None;
    }

    let clause = match target {
        Target::MySql => "AUTO_INCREMENT",
        Target::TSql => "IDENTITY(1,1)",
        Target::Sqlite => "PRIMARY KEY AUTOINCREMENT",
    };
    match (&drawn, target) {
        (Drawn::NextVal(seq), _) => lose(
            clause,
            format!(
                "{} has no sequences; the table's own counter is the nearest thing — it counts from 1 \
                 by 1 whatever `{seq}` did, and nothing else can draw from it",
                target.label()
            ),
        ),
        (Drawn::Identity(IdentityKind::Always), Target::MySql | Target::Sqlite) => lose(
            clause,
            format!(
                "{} takes an explicit value for it where ALWAYS refused one, so the column no longer \
                 guards against ids written by hand",
                target.label()
            ),
        ),
        (Drawn::Identity(IdentityKind::ByDefault) | Drawn::Serial, Target::TSql) => lose(
            clause,
            "SQL Server refuses an explicit value for an IDENTITY column unless SET IDENTITY_INSERT is \
             ON, where PostgreSQL took one — an insert or a data load that supplies the id now fails"
                .to_string(),
        ),
        _ => {}
    }
    Some(clause)
}

/// The `schema.name` a foreign key points at. An unqualified parent resolves to
/// the child's own schema, as `search_path` would have.
fn parent_of(e: &Entity, fk: &ForeignKey) -> String {
    match fk.ref_schema.as_deref().or(e.schema.as_deref()) {
        Some(s) => format!("{s}.{}", fk.ref_table),
        None => fk.ref_table.clone(),
    }
}

/// The columns of `e` that sit in a key: the primary key, a UNIQUE constraint
/// or index, or a foreign key the script keeps.
fn keyed_columns(e: &Entity, td: &TableDef, script: &Script) -> HashSet<String> {
    let mut keyed: HashSet<&str> = td
        .columns
        .iter()
        .filter(|c| c.is_pk || c.is_unique)
        .map(|c| c.name.as_str())
        .collect();
    let kept = |fk: &ForeignKey| script.tables.contains(&parent_of(e, fk));
    for con in &td.constraints {
        match con {
            TableConstraint::PrimaryKey { columns, .. } | TableConstraint::Unique { columns, .. } => {
                keyed.extend(columns.iter().map(String::as_str));
            }
            TableConstraint::ForeignKey(fk) if kept(fk) => keyed.extend(fk.columns.iter().map(String::as_str)),
            _ => {}
        }
    }
    for fk in td
        .columns
        .iter()
        .filter_map(|c| c.inline_fk.as_ref())
        .filter(|fk| kept(fk))
    {
        keyed.extend(fk.columns.iter().map(String::as_str));
    }
    for idx in td.indexes.iter().filter(|i| i.unique) {
        keyed.extend(idx.columns.iter().filter(|k| !k.is_expression).map(|k| k.name.as_str()));
    }
    keyed.into_iter().map(str::to_string).collect()
}

/// A column's emitted type, and what was lost choosing it.
struct ColumnType {
    ty: String,
    notes: Vec<Downgrade>,
}

/// The type each of `td`'s columns is emitted as, in column order, with the
/// losses found choosing it.
///
/// Three passes, because the last two need the first's answer for every
/// column. Each column's type is mapped alone ([`map_type`]). A column in a
/// key whose type the target cannot key — unbounded text or binary — is then
/// bounded: leaving the constraint out would lose the key and every foreign
/// key that points at it, where a bound only refuses a value longer than it. A
/// foreign key's own columns take the same bound as the key they point at, so
/// the two sides still match. An ordinary index is not a reason to change a
/// column's type; one on unbounded text is left out instead ([`emit_index`]).
/// Last, a row wider than the target allows gives up its widest columns'
/// fixed widths ([`fit_row`]).
fn column_types(e: &Entity, td: &TableDef, script: &Script) -> Vec<ColumnType> {
    let target = script.target;
    let keyed = keyed_columns(e, td, script);
    let mut types: Vec<ColumnType> = td
        .columns
        .iter()
        .map(|c| {
            let mut notes = Vec::new();
            let ty = map_type(&c.data_type, script, e, c, &mut notes);
            // A collation is named, and the names are PostgreSQL's: no target
            // has `"C"` or an ICU locale under the same name. The column takes
            // the target's default, which may sort and compare differently.
            if let Some(collation) = &c.collation {
                notes.push(Downgrade {
                    entity: e.name.clone(),
                    column: Some(c.name.clone()),
                    from: format!("`COLLATE {collation}`"),
                    to: "the target's default collation".to_string(),
                    reason: "the collation is PostgreSQL's, by a name the target does not have, so the \
                             column's values may sort, compare and match differently"
                        .to_string(),
                });
            }
            ColumnType { ty, notes }
        })
        .collect();
    for (c, typed) in td.columns.iter().zip(types.iter_mut()) {
        if !keyed.contains(&c.name) {
            continue;
        }
        let Some(why) = unindexable(&typed.ty, target) else {
            continue;
        };
        let to = key_bound(&c.data_type, target);
        typed.notes.push(Downgrade {
            entity: e.name.clone(),
            column: Some(c.name.clone()),
            from: format!("`{}` in a key", c.data_type),
            to: to.to_string(),
            reason: format!(
                "{why} — so the column is bounded to {to} instead, which keeps the key; a longer value is now refused"
            ),
        });
        typed.ty = to.to_string();
    }
    fit_row(e, td, &keyed, target, &mut types);
    types
}

/// Give a row wider than the target allows the room it needs, widest column
/// first.
///
/// Each column is legal alone and the CREATE TABLE is still refused: MySQL
/// counts every VARCHAR and CHAR at four bytes a utf8mb4 character against
/// 65,535 bytes a row, and SQL Server every fixed-length column — nchar among
/// them — against 8,060. A MySQL string becomes TEXT, which counts ten bytes,
/// and a SQL Server nchar becomes nvarchar, which may overflow the row, until
/// the row fits. A key column keeps its type; a column an index reads is taken
/// last, since TEXT would leave the index out. Each change is reported.
fn fit_row(e: &Entity, td: &TableDef, keyed: &HashSet<String>, target: Target, types: &mut [ColumnType]) {
    let limit = match target {
        Target::MySql => MYSQL_ROW_MAX,
        Target::TSql => TSQL_ROW_MAX,
        Target::Sqlite => return,
    };
    let needed = row_bytes(td, types, target);
    if needed <= limit {
        return;
    }
    let indexed: HashSet<&str> = td
        .indexes
        .iter()
        .flat_map(|i| i.columns.iter().filter(|k| !k.is_expression).map(|k| k.name.as_str()))
        .collect();
    let mut order: Vec<usize> = (0..types.len())
        .filter(|&i| !keyed.contains(&td.columns[i].name) && freed(&types[i].ty, target).is_some())
        .collect();
    order.sort_by_key(|&i| {
        (
            indexed.contains(td.columns[i].name.as_str()),
            std::cmp::Reverse(column_row_bytes(&types[i].ty, target)),
            i,
        )
    });
    for i in order {
        if row_bytes(td, types, target) <= limit {
            break;
        }
        let Some((to, n)) = freed(&types[i].ty, target) else {
            continue;
        };
        let c = &td.columns[i];
        let reason = match target {
            Target::MySql => format!(
                "MySQL keeps at most {} bytes of a row outside its TEXT and BLOB columns, counting four bytes a \
                 utf8mb4 character, and this table's columns need {} — so its widest are stored as {to} instead: \
                 a value longer than {} characters is no longer refused{}",
                thousands(MYSQL_ROW_MAX),
                thousands(needed),
                thousands(n),
                if types[i].ty.to_uppercase().starts_with("CHAR(") {
                    ", and is no longer padded to its length"
                } else {
                    ""
                }
            ),
            _ => format!(
                "SQL Server keeps a row's fixed-length columns, nchar among them, within {} bytes, and this \
                 table's need {} — so its widest nchar columns are nvarchar instead, which may overflow the \
                 row: a value is no longer padded to its length",
                thousands(TSQL_ROW_MAX),
                thousands(needed)
            ),
        };
        types[i].notes.push(Downgrade {
            entity: e.name.clone(),
            column: Some(c.name.clone()),
            from: format!("`{}`", c.data_type),
            to: to.clone(),
            reason,
        });
        types[i].ty = to;
    }
}

/// What a column emitted as `ty` becomes to stop counting against the row,
/// and its length — `None` when it already does not count, or cannot stop.
fn freed(ty: &str, target: Target) -> Option<(String, u64)> {
    let lower = ty.trim().to_lowercase();
    let base = lower.split('(').next().unwrap_or(&lower).trim();
    let n = lower.find('(').and_then(|i| length_of(&lower[i..]))?;
    match (target, base) {
        (Target::MySql, "varchar" | "char") => Some((mysql_text_holding(n).to_string(), n)),
        (Target::TSql, "nchar") => Some((format!("nvarchar({n})"), n)),
        _ => None,
    }
}

/// The bytes `td`'s row needs on the target, typed as `types`, against the
/// limit [`fit_row`] holds it to.
///
/// MySQL counts each column's widest value and a bit per nullable column
/// ("Limits on Table Column Count and Row Size": a `VARCHAR(65533) NOT NULL`
/// latin1 column fits, a nullable one does not). SQL Server counts its
/// fixed-length columns, and the row's own overhead: a 4-byte header, a 2-byte
/// column count, the null bitmap, and 2 bytes per variable-length column plus
/// 2 for their count.
fn row_bytes(td: &TableDef, types: &[ColumnType], target: Target) -> u64 {
    let data: u64 = types.iter().map(|t| column_row_bytes(&t.ty, target)).sum();
    match target {
        Target::MySql => data + (td.columns.iter().filter(|c| c.nullable).count() as u64).div_ceil(8),
        Target::TSql => {
            let variable = types.iter().filter(|t| tsql_fixed_bytes(&t.ty).is_none()).count() as u64;
            let offsets = if variable > 0 { 2 + 2 * variable } else { 0 };
            data + 4 + 2 + (types.len() as u64).div_ceil(8) + offsets
        }
        Target::Sqlite => 0,
    }
}

/// What a column emitted as `ty` counts against the target's row limit.
fn column_row_bytes(ty: &str, target: Target) -> u64 {
    match target {
        Target::MySql => mysql_row_bytes(ty),
        Target::TSql => tsql_fixed_bytes(ty).unwrap_or(0),
        Target::Sqlite => 0,
    }
}

/// The bytes a column emitted as `ty` takes of a MySQL row, as MySQL counts
/// them ("Data Type Storage Requirements"): a string at four bytes a utf8mb4
/// character plus a VARCHAR's 1- or 2-byte length; a TEXT, BLOB or JSON column
/// only the 9 to 12 bytes that point at it. Anything not listed is taken at
/// the 12 a pointer takes at most — `emit` writes nothing wider that is not.
fn mysql_row_bytes(ty: &str) -> u64 {
    let upper = ty.trim().to_uppercase();
    let base = upper.split('(').next().unwrap_or(&upper).trim();
    let args = upper.find('(').map_or("", |i| &upper[i..]);
    let n = length_of(args);
    // Nine digits of a DECIMAL pack into four bytes, the rest into these.
    const LEFTOVER: [u64; 10] = [0, 1, 1, 2, 2, 3, 3, 4, 4, 4];
    let digits = |d: i64| {
        let d = d.max(0) as u64;
        (d / 9) * 4 + LEFTOVER[(d % 9) as usize]
    };
    // Fractional seconds take a byte per two digits.
    let fsp = |n: Option<u64>| n.unwrap_or(0).div_ceil(2);
    match base {
        "TINYINT" => 1,
        "SMALLINT" => 2,
        "INT" | "FLOAT" => 4,
        "BIGINT" | "DOUBLE" => 8,
        "DATE" => 3,
        "TIME" => 3 + fsp(n),
        "DATETIME" => 5 + fsp(n),
        "DECIMAL" => {
            let (p, s) = precision_scale(args).unwrap_or((10, 0));
            digits(p - s) + digits(s)
        }
        "CHAR" => n.unwrap_or(1) * MYSQL_BYTES_PER_CHAR,
        "VARCHAR" | "VARBINARY" => {
            let per = if base == "VARCHAR" { MYSQL_BYTES_PER_CHAR } else { 1 };
            let bytes = n.unwrap_or(0) * per;
            bytes + if bytes > 255 { 2 } else { 1 }
        }
        "TINYTEXT" | "TINYBLOB" => 9,
        "TEXT" | "BLOB" => 10,
        "MEDIUMTEXT" | "MEDIUMBLOB" => 11,
        _ => 12,
    }
}

/// The bytes a column emitted as `ty` fixes in a SQL Server row, or `None` for
/// a variable-length type, which may overflow the row (the "Storage size" of
/// each type in "Data types (Transact-SQL)").
fn tsql_fixed_bytes(ty: &str) -> Option<u64> {
    let lower = ty.trim().to_lowercase();
    let base = lower.split('(').next().unwrap_or(&lower).trim();
    let args = lower.find('(').map_or("", |i| &lower[i..]);
    let n = length_of(args);
    Some(match base {
        "bit" | "tinyint" => 1,
        "smallint" => 2,
        "int" | "real" => 4,
        "bigint" | "float" => 8,
        "date" => 3,
        "time" => match n {
            Some(0..=2) => 3,
            Some(3..=4) => 4,
            _ => 5,
        },
        "datetime2" => match n {
            Some(0..=2) => 6,
            Some(3..=4) => 7,
            _ => 8,
        },
        "datetimeoffset" => match n {
            Some(0..=2) => 8,
            Some(3..=4) => 9,
            _ => 10,
        },
        "decimal" | "numeric" => match precision_scale(args).map_or(18, |(p, _)| p) {
            ..=9 => 5,
            10..=19 => 9,
            20..=28 => 13,
            _ => 17,
        },
        "uniqueidentifier" => 16,
        "nchar" => 2 * n.unwrap_or(1),
        "char" | "binary" => n.unwrap_or(1),
        _ => return None,
    })
}

/// The bounded type a key column of PostgreSQL type `pg` gets. SQL Server keys
/// at most 900 bytes — 450 nvarchar characters; MySQL's 255 characters fit a
/// utf8mb4 key three columns wide.
fn key_bound(pg: &str, target: Target) -> &'static str {
    let binary = pg.trim().eq_ignore_ascii_case("bytea");
    match (target, binary) {
        (Target::MySql, true) => "VARBINARY(255)",
        (Target::MySql, false) => "VARCHAR(255)",
        (Target::TSql | Target::Sqlite, true) => "varbinary(900)",
        (Target::TSql | Target::Sqlite, false) => "nvarchar(450)",
    }
}

/// One `FOREIGN KEY … REFERENCES …` clause. An unqualified parent resolves to
/// the child's own schema, as `search_path` would have.
///
/// `None` when the parent is not in the script — declared `external:`, or left
/// out by `--scope`. SQL Server and MySQL refuse a key to a table that does not
/// exist, and SQLite accepts one and then fails every insert, so the key is
/// left out and reported: the script has to apply on its own.
fn foreign_key(e: &Entity, fk: &ForeignKey, script: &Script, report: &mut Vec<Downgrade>) -> Option<String> {
    let target = script.target;
    let parent_schema = fk.ref_schema.as_deref().or(e.schema.as_deref());
    let parent_name = parent_of(e, fk);
    if !script.tables.contains(&parent_name) {
        let why = if script.externals.contains(&parent_name) {
            format!(
                "`{parent_name}` is declared `external:` — managed outside this project, so the script does not create it"
            )
        } else {
            format!("`{parent_name}` is not in the emitted script (outside the scope, or not part of the project)")
        };
        report.push(Downgrade {
            entity: e.name.clone(),
            column: None,
            from: format!("a foreign key ({}) to `{parent_name}`", fk.columns.join(", ")),
            to: "no foreign key".to_string(),
            reason: format!(
                "{why}; a key to a table that is not there would fail the script, so it is left \
                 out — add it once the parent exists"
            ),
        });
        return None;
    }
    let parent = qualified(parent_schema, &fk.ref_table, target);
    let mut sql = match &fk.name {
        Some(n) => format!("CONSTRAINT {} ", target.quote(n)),
        None => String::new(),
    };
    sql.push_str(&format!(
        "FOREIGN KEY ({}) REFERENCES {parent} ({})",
        quote_all(&fk.columns, target),
        quote_all(&fk.ref_columns, target)
    ));
    // `MATCH FULL` goes right after the referenced columns. No target enforces
    // it: SQLite parses it and treats every key as `MATCH SIMPLE`, MySQL
    // ignores it, SQL Server has no such clause — so a half-NULL key the
    // PostgreSQL design refuses would pass.
    if fk.match_full {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: fk.columns.first().cloned(),
            from: format!("a `MATCH FULL` foreign key ({})", fk.columns.join(", ")),
            to: "a `MATCH SIMPLE` one".to_string(),
            reason: "the target does not enforce MATCH FULL, so a key with some columns NULL and \
                     others not is no longer refused"
                .to_string(),
        });
    }
    for (verb, action) in [("DELETE", fk.on_delete), ("UPDATE", fk.on_update)] {
        let Some(action) = action else { continue };
        match fk_action(action, target) {
            Some(keyword) => sql.push_str(&format!(" ON {verb} {keyword}")),
            None => report.push(Downgrade {
                entity: e.name.clone(),
                column: fk.columns.first().cloned(),
                from: format!("`ON {verb} SET DEFAULT`"),
                to: "the default action".to_string(),
                reason: "MySQL's InnoDB rejects SET DEFAULT, so referencing rows are no longer \
                         reset when the parent row changes"
                    .to_string(),
            }),
        }
    }
    // SQLite defers a key exactly as `INITIALLY DEFERRED` does. A key that is
    // deferrable but checked at once by default needs `SET CONSTRAINTS`, which
    // SQLite lacks; MySQL and SQL Server defer nothing.
    if fk.deferrable || fk.initially_deferred {
        match target {
            Target::Sqlite if fk.initially_deferred => sql.push_str(" DEFERRABLE INITIALLY DEFERRED"),
            _ => report.push(Downgrade {
                entity: e.name.clone(),
                column: fk.columns.first().cloned(),
                from: format!(
                    "a `DEFERRABLE{}` foreign key ({})",
                    if fk.initially_deferred {
                        " INITIALLY DEFERRED"
                    } else {
                        ""
                    },
                    fk.columns.join(", ")
                ),
                to: "one checked at once".to_string(),
                reason: "the target cannot put the check off to the end of the transaction, so a \
                         child row inserted before its parent is refused"
                    .to_string(),
            }),
        }
    }
    Some(sql)
}

/// The target's keyword for a referential action; `None` when it has none.
fn fk_action(action: FkAction, target: Target) -> Option<&'static str> {
    match (action, target) {
        (FkAction::Cascade, _) => Some("CASCADE"),
        (FkAction::SetNull, _) => Some("SET NULL"),
        (FkAction::NoAction, _) => Some("NO ACTION"),
        // SQL Server has no RESTRICT keyword, and its NO ACTION is checked at
        // once — which is what RESTRICT means. Faithful, so not reported.
        (FkAction::Restrict, Target::TSql) => Some("NO ACTION"),
        (FkAction::Restrict, _) => Some("RESTRICT"),
        (FkAction::SetDefault, Target::MySql) => None,
        (FkAction::SetDefault, _) => Some("SET DEFAULT"),
    }
}

/// Why the target cannot index a column emitted as `ty` — an unbounded text,
/// binary or JSON type.
///
/// Read off the emitted type, not PostgreSQL's: an unbounded `varchar`, a
/// `varchar(70000)` and a column [`fit_row`] moved to TEXT are all unbounded
/// here, whatever PostgreSQL called them.
fn unindexable(ty: &str, target: Target) -> Option<&'static str> {
    let upper = ty.trim().to_uppercase();
    let base = upper.split('(').next().unwrap_or(&upper).trim();
    match target {
        Target::MySql
            if matches!(
                base,
                "TINYTEXT"
                    | "TEXT"
                    | "MEDIUMTEXT"
                    | "LONGTEXT"
                    | "TINYBLOB"
                    | "BLOB"
                    | "MEDIUMBLOB"
                    | "LONGBLOB"
                    | "JSON"
            ) =>
        {
            Some("MySQL cannot index a TEXT, BLOB or JSON column without a prefix length")
        }
        Target::TSql if upper.ends_with("(MAX)") => {
            Some("SQL Server cannot index an nvarchar(max) or varbinary(max) column")
        }
        _ => None,
    }
}

fn index_type_label(t: &IndexType) -> String {
    match t {
        IndexType::Btree => "B-tree".to_string(),
        IndexType::Hash => "hash".to_string(),
        IndexType::Gin => "GIN".to_string(),
        IndexType::Gist => "GiST".to_string(),
        IndexType::Brin => "BRIN".to_string(),
        IndexType::SpGist => "SP-GiST".to_string(),
        IndexType::Other(m) => m.clone(),
    }
}

/// A `CREATE INDEX` for each of the table's indexes, with a note above each
/// loss. An index the target would reject outright is left out — reported, so
/// the omission is visible, and absent, so the script still applies.
fn emit_indexes(e: &Entity, script: &Script, report: &mut Vec<Downgrade>) -> Vec<String> {
    let target = script.target;
    let Some(td) = &e.table_def else { return Vec::new() };
    let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
    let table = qualified(e.schema.as_deref(), bare, target);
    // Whether the target can index a column depends on the type it is
    // emitted as — bounded for a key, or moved to TEXT to fit the row.
    let types: Vec<String> = column_types(e, td, script).into_iter().map(|t| t.ty).collect();
    td.indexes
        .iter()
        .filter_map(|idx| emit_index(e, td, idx, bare, &table, &types, target, report))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn emit_index(
    e: &Entity,
    td: &crate::entity::TableDef,
    idx: &IndexDef,
    bare: &str,
    table: &str,
    types: &[String],
    target: Target,
    report: &mut Vec<Downgrade>,
) -> Option<String> {
    let name = idx.name.clone().unwrap_or_else(|| {
        let cols: Vec<&str> = idx.columns.iter().map(|c| c.name.as_str()).collect();
        format!("{bare}_{}_idx", cols.join("_"))
    });
    let partial = idx
        .predicate
        .as_ref()
        .map(|p| format!(" WHERE {p}"))
        .unwrap_or_default();
    let loss = |from: String, to: &str, reason: String| Downgrade {
        entity: e.name.clone(),
        column: None,
        from,
        to: to.to_string(),
        reason,
    };

    // What would make the target reject the statement.
    let mut blockers: Vec<String> = Vec::new();
    for col in &idx.columns {
        if col.is_expression {
            if target == Target::TSql {
                blockers.push("SQL Server cannot key an index on an expression without a computed column".into());
            }
        } else if let Some(ty) = td
            .columns
            .iter()
            .position(|c| c.name == col.name)
            .and_then(|i| types.get(i))
            && let Some(why) = unindexable(ty, target)
        {
            blockers.push(format!("{why} (`{}` is {ty} here)", col.name));
        }
    }
    if !blockers.is_empty() {
        let keys: Vec<String> = idx
            .columns
            .iter()
            .map(|c| {
                if c.is_expression {
                    format!("expression `{}`", c.name)
                } else {
                    c.name.clone()
                }
            })
            .collect();
        let d = loss(
            format!("the index `{name}` on ({}){partial}", keys.join(", ")),
            "no index",
            format!("{} — it is left out so the script still applies", blockers.join("; ")),
        );
        let note = d.comment(target);
        report.push(d);
        return Some(note);
    }

    let before = report.len();
    let mut keys = Vec::new();
    for col in &idx.columns {
        if col.is_expression {
            report.push(loss(
                format!("an index key expression `{}`", col.name),
                "the same text, untranslated",
                "dbd translates types and structure, not expressions — check it against the target \
                 by hand (MySQL also rejects one that yields TEXT or BLOB)"
                    .into(),
            ));
            keys.push(format!("({})", col.name));
            continue;
        }
        let mut key = target.quote(&col.name);
        if col.order == Some(SortOrder::Desc) {
            key.push_str(" DESC");
        }
        if let Some(first) = col.nulls_first {
            let spelled = if first { "NULLS FIRST" } else { "NULLS LAST" };
            if target == Target::Sqlite {
                key.push_str(&format!(" {spelled}"));
            } else {
                report.push(loss(
                    format!("`{spelled}` on `{}`", col.name),
                    "the engine's own NULL ordering",
                    format!("{} has no NULLS FIRST/LAST in an index", target.label()),
                ));
            }
        }
        if let Some(op) = &col.opclass {
            report.push(loss(
                format!("the operator class `{op}` on `{}`", col.name),
                "the default comparison",
                "operator classes are PostgreSQL's; a lookup that relied on this one may not use the index".into(),
            ));
        }
        keys.push(key);
    }
    if let Some(t) = &idx.index_type
        && *t != IndexType::Btree
    {
        let label = index_type_label(t);
        report.push(loss(
            format!("a {label} index"),
            "an ordinary index",
            format!(
                "{} has no {label} access method, so queries it served may not use this one",
                target.label()
            ),
        ));
    }
    let mut tail = String::new();
    if let Some(p) = &idx.predicate {
        if target == Target::MySql {
            report.push(loss(
                format!("a partial index `WHERE {p}`"),
                "an index over every row",
                "MySQL has no partial indexes, so it grows with every row — and a UNIQUE one now \
                 constrains rows it did not"
                    .into(),
            ));
        } else {
            report.push(loss(
                format!("a partial index `WHERE {p}`"),
                "the same predicate, untranslated",
                "dbd translates types and structure, not expressions — check the predicate against \
                 the target by hand"
                    .into(),
            ));
            tail.push_str(&format!(" WHERE {p}"));
        }
    }
    if !idx.include.is_empty() {
        if target == Target::TSql {
            tail = format!(" INCLUDE ({}){tail}", quote_all(&idx.include, target));
        } else {
            report.push(loss(
                format!("`INCLUDE ({})`", idx.include.join(", ")),
                "nothing",
                format!("{} has no covering-index payload columns", target.label()),
            ));
        }
    }
    // SQL Server's unique index already lets only one NULL in; the other two
    // treat NULLs as distinct, so two NULLs no longer collide.
    if idx.nulls_not_distinct && target != Target::TSql {
        report.push(loss(
            "`NULLS NOT DISTINCT`".into(),
            "NULLs that do not collide",
            "the index now admits rows with NULL keys that it refused".into(),
        ));
    }
    if !idx.with_options.is_empty() {
        let opts: Vec<String> = idx.with_options.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        report.push(loss(
            format!("storage parameters `WITH ({})`", opts.join(", ")),
            "none",
            "they tune PostgreSQL's access method and mean nothing here".into(),
        ));
    }

    let mut out: Vec<String> = report[before..].iter().map(|d| d.comment(target)).collect();
    let unique = if idx.unique { "UNIQUE " } else { "" };
    out.push(format!(
        "CREATE {unique}INDEX {} ON {table} ({}){tail};",
        target.quote(&name),
        keys.join(", ")
    ));
    Some(out.join("\n"))
}

/// A view — or a materialized view, as the plain view that is the nearest
/// thing — with `views` gaining it once it is created.
///
/// On SQLite the body is written again from the parse tree, or the view is
/// left out; see [`sqlite`] for why that and not a copy. Elsewhere it goes
/// across as written, and is reported.
fn emit_view(e: &Entity, script: &Script, views: &mut HashSet<String>, report: &mut Vec<Downgrade>) -> Option<String> {
    let target = script.target;
    let lost = |from: String, to: &str, reason: String| Downgrade {
        entity: e.name.clone(),
        column: None,
        from,
        to: to.to_string(),
        reason,
    };
    let Some(written) = e.body.first() else {
        let d = lost(
            format!("a {} whose body dbd could not read", e.entity_type.tag()),
            "nothing",
            "there is no SELECT to emit, so it is missing from the script".to_string(),
        );
        let note = d.comment(target);
        report.push(d);
        return Some(note);
    };
    let body = match target {
        Target::Sqlite => match sqlite::query(written, &|schema, name| script.relation(e, schema, name, views)) {
            Ok(body) => body,
            Err(sqlite::NoForm(why)) => {
                let kind = match e.entity_type {
                    EntityType::MaterializedView => "a materialized view",
                    _ => "a view",
                };
                let d = lost(
                    kind.to_string(),
                    "nothing",
                    format!("{why} — so it is left out and the script applies; anything that reads it is left out too"),
                );
                let note = d.comment(target);
                report.push(d);
                return Some(note);
            }
        },
        Target::MySql | Target::TSql => written.clone(),
    };
    let name = table_name(e, target, report);
    let before = report.len();

    // None of the three has materialized views. A plain view is the nearest
    // thing — the same rows, computed on every read instead of stored at the
    // last REFRESH — and its indexes have nothing to index.
    if e.entity_type == EntityType::MaterializedView {
        report.push(lost(
            "a materialized view".to_string(),
            "a plain view",
            format!(
                "{} has no materialized views, so the query runs on every read instead of serving \
                 the rows stored at the last REFRESH — reads cost what the query costs, and there is \
                 nothing to refresh",
                target.label()
            ),
        ));
        let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
        for idx in e.table_def.iter().flat_map(|td| &td.indexes) {
            let name = idx.name.clone().unwrap_or_else(|| {
                let cols: Vec<&str> = idx.columns.iter().map(|c| c.name.as_str()).collect();
                format!("{bare}_{}_idx", cols.join("_"))
            });
            let why = match target {
                // SQL Server does index a view, but only one created WITH
                // SCHEMABINDING over a body written for it — not an
                // untranslated one.
                Target::TSql => {
                    "SQL Server indexes only a view created WITH SCHEMABINDING over a body \
                                 written for it, which this untranslated one is not"
                }
                Target::MySql => "MySQL cannot index a view",
                Target::Sqlite => "SQLite cannot index a view",
            };
            report.push(lost(
                format!("the index `{name}` on the materialized view"),
                "no index",
                format!("{why} — it is left out so the script still applies"),
            ));
        }
    }
    if let Some(text) = e.comment.as_deref()
        && target != Target::TSql
    {
        report.push(comment_lost(e, None, text, target));
    }
    report.push(match target {
        Target::Sqlite => lost(
            "a view body in PostgreSQL SQL".to_string(),
            "the same query in SQLite's SQL",
            SQLITE_REWRITTEN.to_string(),
        ),
        Target::MySql | Target::TSql => lost(
            "a view body in PostgreSQL SQL".to_string(),
            "the same text, untranslated",
            "dbd translates types and structure, not expressions — anything PostgreSQL-specific inside \
             the SELECT has to be checked by hand"
                .to_string(),
        ),
    });
    let mut out: Vec<String> = report[before..].iter().map(|d| d.comment(target)).collect();
    out.push(format!("CREATE VIEW {name} AS {body};"));
    views.insert(e.name.clone());
    Some(out.join("\n"))
}

/// Why a view body or CHECK written again in SQLite's SQL is still reported:
/// the text is SQLite's, and the engine under it is not PostgreSQL.
const SQLITE_REWRITTEN: &str = "dbd wrote it again from PostgreSQL's parse tree — table names flattened, \
     functions and operators mapped to SQLite's own — but the engines differ even where the SQL reads the \
     same: LIKE ignores ASCII case, upper() and lower() change ASCII letters only, and values compare by \
     column affinity — so the results are worth checking by hand";

/// A sequence: `CREATE SEQUENCE` on SQL Server; everywhere else, a note where
/// it would have been, and a report entry.
///
/// SQL Server's defaults are not PostgreSQL's — its MINVALUE, and so its first
/// value, is the type's minimum (-2^63 for bigint), and it caches by default —
/// so every bound is resolved to what PostgreSQL would have used and spelled
/// out. The options come from the file's own `CREATE SEQUENCE`, read through
/// libpg_query, because the model keeps none.
fn emit_sequence(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> Option<String> {
    if target != Target::TSql {
        let d = Downgrade {
            entity: e.name.clone(),
            column: None,
            from: "a sequence".to_string(),
            to: "nothing".to_string(),
            reason: format!(
                "{} has no sequences — a column that drew its default from this one is reported \
                 where it is",
                target.label()
            ),
        };
        let note = d.comment(target);
        report.push(d);
        return Some(note);
    }

    let name = table_name(e, target, report);
    let before = report.len();
    let read = e
        .file
        .as_deref()
        .and_then(|f| crate::source_text::read_to_string(f).ok())
        .and_then(|sql| crate::parser::pg::sequences::sequence_options(&sql));
    if read.is_none() {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: None,
            from: "a sequence whose options dbd could not read back".to_string(),
            to: "PostgreSQL's defaults".to_string(),
            reason: "the file did not yield its CREATE SEQUENCE again, so any START, INCREMENT or \
                     bound it set is not carried"
                .to_string(),
        });
    }
    let o = read.unwrap_or_default();

    let (ty, lowest, highest) = match o.data_type.as_deref() {
        Some("int2" | "smallint") => ("smallint", i64::from(i16::MIN), i64::from(i16::MAX)),
        Some("int4" | "integer" | "int") => ("int", i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => ("bigint", i64::MIN, i64::MAX),
    };
    let increment = o.increment.unwrap_or(1);
    let ascending = increment > 0;
    let min = o.min.flatten().unwrap_or(if ascending { 1 } else { lowest });
    let max = o.max.flatten().unwrap_or(if ascending { highest } else { -1 });
    let start = o.start.unwrap_or(if ascending { min } else { max });
    let cycle = if o.cycle.unwrap_or(false) { "CYCLE" } else { "NO CYCLE" };
    // PostgreSQL's CACHE 1 hands out one value at a time, which is SQL
    // Server's NO CACHE; its own default would skip numbers after a restart.
    let cache = match o.cache.unwrap_or(1) {
        n if n > 1 => format!("CACHE {n}"),
        _ => "NO CACHE".to_string(),
    };

    if let Some(owner) = &o.owned_by {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: None,
            from: format!("`OWNED BY {owner}`"),
            to: "a sequence of its own".to_string(),
            reason: "SQL Server cannot tie a sequence to a column, so dropping the column no longer \
                     drops the sequence"
                .to_string(),
        });
    }

    let mut out: Vec<String> = report[before..].iter().map(|d| d.comment(target)).collect();
    out.push(format!(
        "CREATE SEQUENCE {name} AS {ty} START WITH {start} INCREMENT BY {increment} MINVALUE {min} \
         MAXVALUE {max} {cycle} {cache};"
    ));
    Some(out.join("\n"))
}

fn quote_all(columns: &[String], target: Target) -> String {
    columns.iter().map(|c| target.quote(c)).collect::<Vec<_>>().join(", ")
}

/// PostgreSQL's type as the target's, recording the loss when there is one.
fn map_type(pg: &str, script: &Script, e: &Entity, c: &ColumnDef, report: &mut Vec<Downgrade>) -> String {
    let target = script.target;
    let label = target.label();
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

    // PostgreSQL writes a zone after any precision — `time (3) with time zone`
    // — so the zone is read off the end before the type is cut at its
    // parenthesis. Cut first, it was a plain `time` or `timestamp`, and SQL
    // Server got a `datetime2` with no offset.
    let (t, zoned) = match t.strip_suffix(" with time zone") {
        Some(rest) => (rest.trim(), true),
        None => (t.strip_suffix(" without time zone").unwrap_or(&t).trim(), false),
    };
    // `varchar(20)` keeps its length; match on the base.
    let base = match (t.split('(').next().unwrap_or(t).trim(), zoned) {
        ("time", true) => "timetz",
        ("timestamp", true) => "timestamptz",
        (base, _) => base,
    };
    let args = t.find('(').map(|i| t[i..].trim()).unwrap_or("");

    match (base, target) {
        ("jsonb", Target::MySql) => "JSON".to_string(),
        // MySQL normalizes a JSON document as `jsonb` does; `json` keeps the
        // text it was given, so only `json` loses something here.
        ("json", Target::MySql) => lose(
            "JSON",
            "MySQL's JSON stores a document normalized — keys sorted, a duplicated key kept once, \
             whitespace dropped — where `json` keeps the text as written, so a document reads back \
             differently from how it was stored",
        ),
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

        // PostgreSQL's text holds up to a gigabyte. MySQL's TEXT holds 65,535
        // bytes — as few as 16,383 utf8mb4 characters — and refused a longer
        // value without a word in the report; LONGTEXT holds 4 GB ("String
        // Type Storage Requirements", MySQL 8.4 manual).
        ("text", target) => unbounded_text(target).to_string(),

        ("varchar" | "character varying", _) => settle(varchar(args, target), &mut lose),

        // The serial types are integers numbered by a sequence; the numbering is
        // the column's, and `numbering` carries it.
        ("integer" | "int" | "int4" | "serial" | "serial4", Target::MySql) => "INT".to_string(),
        ("integer" | "int" | "int4" | "serial" | "serial4", Target::TSql) => "int".to_string(),
        ("integer" | "int" | "int4" | "serial" | "serial4", Target::Sqlite) => "INTEGER".to_string(),

        ("bigint" | "int8" | "bigserial" | "serial8", Target::MySql) => "BIGINT".to_string(),
        ("bigint" | "int8" | "bigserial" | "serial8", Target::TSql) => "bigint".to_string(),
        ("bigint" | "int8" | "bigserial" | "serial8", Target::Sqlite) => "INTEGER".to_string(),

        ("smallint" | "int2" | "smallserial" | "serial2", Target::MySql) => "SMALLINT".to_string(),
        ("smallint" | "int2" | "smallserial" | "serial2", Target::TSql) => "smallint".to_string(),
        ("smallint" | "int2" | "smallserial" | "serial2", Target::Sqlite) => "INTEGER".to_string(),

        ("boolean" | "bool", Target::MySql) => "TINYINT(1)".to_string(),
        ("boolean" | "bool", Target::TSql) => "bit".to_string(),
        ("boolean" | "bool", Target::Sqlite) => "INTEGER".to_string(),

        ("numeric" | "decimal", _) => settle(decimal(args, target), &mut lose),

        ("double precision" | "float8", Target::MySql) => "DOUBLE".to_string(),
        ("double precision" | "float8", Target::TSql) => "float".to_string(),
        ("double precision" | "float8", Target::Sqlite) => "REAL".to_string(),

        // A timestamp keeps its fractional seconds: MySQL's DATETIME defaults
        // to whole seconds (see `mysql_fsp`); SQL Server's types default to
        // seven digits, finer than PostgreSQL's six, so only a declared
        // precision is carried there.
        ("timestamptz" | "timestamp with time zone", Target::MySql) => format!("DATETIME{}", mysql_fsp(args)),
        ("timestamptz" | "timestamp with time zone", Target::TSql) => format!("datetimeoffset{args}"),
        ("timestamptz" | "timestamp with time zone", Target::Sqlite) => "TEXT".to_string(),

        ("timestamp" | "timestamp without time zone", Target::MySql) => format!("DATETIME{}", mysql_fsp(args)),
        ("timestamp" | "timestamp without time zone", Target::TSql) => format!("datetime2{args}"),
        ("timestamp" | "timestamp without time zone", Target::Sqlite) => "TEXT".to_string(),

        ("date", Target::MySql) => "DATE".to_string(),
        ("date", Target::TSql) => "date".to_string(),
        ("date", Target::Sqlite) => "TEXT".to_string(),

        // BLOB holds 65,535 bytes and bytea a gigabyte; LONGBLOB holds 4 GB.
        ("bytea", Target::MySql) => "LONGBLOB".to_string(),
        ("bytea", Target::TSql) => "varbinary(max)".to_string(),
        ("bytea", Target::Sqlite) => "BLOB".to_string(),

        // Blank-padded to its length on both targets that have it; PostgreSQL
        // spells a bare `char` as `char(1)`. A bare `bpchar` has no length at
        // all, so it is not this.
        ("char" | "character" | "bpchar", _) if !args.is_empty() => {
            let n = length_of(args).unwrap_or(1);
            match target {
                Target::MySql if n <= 255 => format!("CHAR({n})"),
                Target::MySql if n <= MYSQL_VARCHAR_MAX => lose(
                    &format!("VARCHAR({n})"),
                    "MySQL's CHAR holds at most 255 characters, so it is a VARCHAR — no longer padded to its length",
                ),
                Target::MySql => {
                    let to = mysql_text_holding(n);
                    lose(
                        to,
                        &format!(
                            "MySQL's CHAR holds at most 255 characters and its VARCHAR {} in utf8mb4, so it is \
                             a {to} — no longer padded to its length, and a value longer than {} characters is \
                             no longer refused",
                            thousands(MYSQL_VARCHAR_MAX),
                            thousands(n)
                        ),
                    )
                }
                Target::TSql if n <= TSQL_NCHAR_MAX => format!("nchar({n})"),
                Target::TSql => lose(
                    "nvarchar(max)",
                    "SQL Server's nchar holds at most 4000 characters, so it is an nvarchar(max) — no longer \
                     padded to its length",
                ),
                Target::Sqlite => "TEXT".to_string(),
            }
        }

        // Single precision everywhere.
        ("real" | "float4", Target::MySql) => "FLOAT".to_string(),
        ("real" | "float4", Target::TSql) => "real".to_string(),
        ("real" | "float4", Target::Sqlite) => "REAL".to_string(),

        // A time of day, its fractional precision kept; SQLite keeps every date
        // and time as text, as it does `date`.
        ("time", Target::MySql) => format!("TIME{}", mysql_fsp(args)),
        ("time", Target::TSql) => format!("time{args}"),
        ("time", Target::Sqlite) => "TEXT".to_string(),

        // Neither MySQL nor SQL Server keeps a time of day with its offset.
        ("timetz", Target::MySql) => lose(
            &format!("TIME{}", mysql_fsp(args)),
            "MySQL's TIME has no time zone, so a value's offset is not kept — times from different \
             zones now compare as if they shared one",
        ),
        ("timetz", Target::TSql) => lose(
            &format!("time{args}"),
            "SQL Server's time has no time zone, so a value's offset is not kept — times from different \
             zones now compare as if they shared one",
        ),
        ("timetz", Target::Sqlite) => "TEXT".to_string(),

        // `interval day to second` names its fields after the type.
        (b, _) if b == "interval" || b.starts_with("interval ") => lose(
            &text_of(target, 255),
            &format!(
                "{label} has no interval type, so it is stored as its text form — adding it to a date, \
                 or comparing two of them, no longer works"
            ),
        ),

        ("inet" | "cidr", _) => lose(
            // 49: the longest text form PostgreSQL writes — an IPv6 address
            // with an IPv4 one inside it (45 characters) and a `/128` netmask.
            &text_of(target, 49),
            &format!(
                "{label} has no type for a network address, so it is stored as its text form — an \
                 address is no longer validated, and PostgreSQL's containment operators (<<, >>=) \
                 have nothing to work on"
            ),
        ),

        // Anything left is a type dbd does not map — an enum when the project
        // declares one by that name, whose labels the target's text column no
        // longer enforces. Text plus a note is the honest floor; a CHECK would
        // need the labels threaded here. Only a declared enum is called one:
        // `money` reported as "if it is an enum" sent readers looking for an
        // enum that does not exist.
        _ if script.is_enum(base) => lose(
            &text_of(target, 255),
            &format!("`{base}` is an enum, and {label} gets it as text — the allowed values are no longer enforced"),
        ),
        _ => lose(
            &text_of(target, 255),
            &format!(
                "dbd has no {label} mapping for `{base}`, so it is stored as text — whatever the type \
                 validated or computed is not carried"
            ),
        ),
    }
}

/// A text column of up to `n` characters on `target` — SQLite's TEXT has no
/// length.
fn text_of(target: Target, n: u32) -> String {
    match target {
        Target::MySql => format!("VARCHAR({n})"),
        Target::TSql => format!("nvarchar({n})"),
        Target::Sqlite => "TEXT".to_string(),
    }
}

// ── The targets' documented limits ──────────────────────────────────────────
//
// `emit` cannot run the target, so a limit it does not know is DDL the target
// refuses — `nvarchar(5000)`, `DECIMAL(50,40)`, `VARCHAR(70000)` — or a type
// that quietly cuts values short. Each limit below is the documented one, so a
// type is switched to the target's larger type, or clamped, exactly where the
// target would otherwise refuse or cut, and the change is reported.

/// MySQL counts a string at its character set's widest, and dbd assumes
/// utf8mb4 — the server default since MySQL 8.0 — at four bytes a character
/// ("The utf8mb4 Character Set", MySQL 8.4 Reference Manual).
const MYSQL_BYTES_PER_CHAR: u64 = 4;

/// The longest VARCHAR MySQL creates in utf8mb4: 65,535 bytes over four bytes a
/// character. A longer one is refused with "Column length too big … (max =
/// 16383); use BLOB or TEXT instead" ("The CHAR and VARCHAR Types").
const MYSQL_VARCHAR_MAX: u64 = 16_383;

/// The most a MySQL row holds outside its TEXT and BLOB columns, which count 9
/// to 12 bytes each; a wider table is refused with "Row size too large" ("Limits
/// on Table Column Count and Row Size").
const MYSQL_ROW_MAX: u64 = 65_535;

/// MySQL's DECIMAL: at most 65 digits, at most 30 of them after the point
/// ("Fixed-Point Types (Exact Value) - DECIMAL, NUMERIC").
const MYSQL_DECIMAL: (i64, i64) = (65, 30);

/// SQL Server's nchar(n) and nvarchar(n) take n from 1 to 4,000; beyond that
/// there is only nvarchar(max), which holds 2^31-1 bytes ("nchar and nvarchar
/// (Transact-SQL)").
const TSQL_NCHAR_MAX: u64 = 4_000;

/// SQL Server's decimal: at most 38 digits, its scale at most its precision
/// ("decimal and numeric (Transact-SQL)").
const TSQL_DECIMAL: (i64, i64) = (38, 38);

/// The most a SQL Server row's fixed-length columns — nchar among them — may
/// take; only variable-length ones may overflow it ("Maximum capacity
/// specifications for SQL Server": bytes per row).
const TSQL_ROW_MAX: u64 = 8_060;

/// `n` as the documentation writes it — `16,383`, `65,535`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

/// A type, and why choosing it loses something — `None` when nothing is lost.
type Mapped = (String, Option<String>);

/// The type of a [`Mapped`], its loss recorded through `lose` when it has one.
fn settle(mapped: Mapped, lose: &mut impl FnMut(&str, &str) -> String) -> String {
    match mapped {
        (ty, Some(reason)) => lose(&ty, &reason),
        (ty, None) => ty,
    }
}

/// The `n` of a `(n)` type modifier.
fn length_of(args: &str) -> Option<u64> {
    args.strip_prefix('(')?.strip_suffix(')')?.trim().parse().ok()
}

/// The target's text type without a length — what PostgreSQL's `text` and an
/// unbounded `varchar` are, neither of which limits a value short of a
/// gigabyte.
fn unbounded_text(target: Target) -> &'static str {
    match target {
        Target::MySql => "LONGTEXT",
        Target::TSql => "nvarchar(max)",
        Target::Sqlite => "TEXT",
    }
}

/// The smallest MySQL text type that holds `chars` utf8mb4 characters: TEXT
/// holds 2^16-1 bytes, MEDIUMTEXT 2^24-1 and LONGTEXT 2^32-1 ("String Type
/// Storage Requirements").
fn mysql_text_holding(chars: u64) -> &'static str {
    match chars * MYSQL_BYTES_PER_CHAR {
        b if b < 1 << 16 => "TEXT",
        b if b < 1 << 24 => "MEDIUMTEXT",
        _ => "LONGTEXT",
    }
}

/// `varchar(n)` — or an unbounded `varchar` — on the target.
fn varchar(args: &str, target: Target) -> Mapped {
    let Some(n) = length_of(args) else {
        // Unbounded in PostgreSQL, so unbounded here: it was silently cut to
        // 255 characters.
        return (unbounded_text(target).to_string(), None);
    };
    match target {
        Target::MySql if n <= MYSQL_VARCHAR_MAX => (format!("VARCHAR({n})"), None),
        Target::MySql => {
            let to = mysql_text_holding(n);
            (
                to.to_string(),
                Some(format!(
                    "MySQL's VARCHAR holds at most {} utf8mb4 characters, so it is a {to} — a value longer \
                     than {} characters is no longer refused",
                    thousands(MYSQL_VARCHAR_MAX),
                    thousands(n)
                )),
            )
        }
        Target::TSql if n <= TSQL_NCHAR_MAX => (format!("nvarchar({n})"), None),
        Target::TSql => (
            "nvarchar(max)".to_string(),
            Some(format!(
                "SQL Server's nvarchar holds at most {} characters, so it is an nvarchar(max) — a value \
                 longer than {} characters is no longer refused",
                thousands(TSQL_NCHAR_MAX),
                thousands(n)
            )),
        ),
        Target::Sqlite => ("TEXT".to_string(), None),
    }
}

/// The `(p)` or `(p,s)` of a numeric type; the scale defaults to 0, as
/// PostgreSQL's does.
fn precision_scale(args: &str) -> Option<(i64, i64)> {
    let inner = args.strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.split(',').map(|part| part.trim().parse::<i64>());
    let p = parts.next()?.ok()?;
    let s = match parts.next() {
        Some(s) => s.ok()?,
        None => 0,
    };
    Some((p, s))
}

/// `10^e` written out — `100`, `0.01`.
fn power_of_ten(e: i64) -> String {
    if e >= 0 {
        format!("1{}", "0".repeat(e as usize))
    } else {
        format!("0.{}1", "0".repeat((-e - 1) as usize))
    }
}

/// What rounding a value to `scale` decimal places reads as.
fn rounded_to(scale: i64) -> String {
    if scale == 0 {
        "a value is rounded to a whole number".to_string()
    } else {
        format!("a value is rounded to {scale} decimal places")
    }
}

/// `numeric(p,s)` — or an unbounded `numeric` — on the target.
///
/// A precision or scale the target allows goes across as written. Otherwise
/// the digits before the point are kept first, since losing one refuses a
/// value outright where losing a decimal place only rounds it; PostgreSQL's
/// negative scale and its scale above the precision (both PostgreSQL 15) have
/// no counterpart and become the nearest ordinary decimal.
fn decimal(args: &str, target: Target) -> Mapped {
    let declared = precision_scale(args);
    let (name, (max_p, max_s), (up, us)) = match target {
        Target::Sqlite => return ("NUMERIC".to_string(), sqlite_numeric_loss(declared)),
        Target::MySql => ("DECIMAL", MYSQL_DECIMAL, MYSQL_DECIMAL),
        // SQL Server's widest decimal keeps 10 places: a choice, reported.
        Target::TSql => ("decimal", TSQL_DECIMAL, (38, 10)),
    };
    let label = target.label();
    let holds = if max_s < max_p {
        format!("at most {max_p} digits, {max_s} of them after the point")
    } else {
        format!("at most {max_p} digits")
    };
    let Some((p, s)) = declared else {
        let to = format!("{name}({up},{us})");
        return (
            to.clone(),
            Some(format!(
                "a numeric with no precision keeps any number of digits, and {label}'s {name} holds {holds}, \
                 so it is {to} — a value with more than {} digits before the point is refused, and one with \
                 more than {us} after it is rounded",
                up - us
            )),
        );
    };

    let mut losses = Vec::new();
    let (p1, s1) = if s < 0 {
        losses.push(format!(
            "a value is no longer rounded to a multiple of {} on the way in, since {label}'s scale cannot be \
             negative",
            power_of_ten(-s)
        ));
        (p - s, 0)
    } else if s > p {
        losses.push(format!(
            "a value from {} up to 1 is now accepted, since {label}'s scale cannot exceed its precision",
            power_of_ten(p - s)
        ));
        (s, s)
    } else {
        (p, s)
    };
    let before_point = p1 - s1;
    let kept_before = before_point.min(max_p);
    let kept_after = s1.min(max_s).min(max_p - kept_before);
    if losses.is_empty() && kept_before == before_point && kept_after == s1 {
        return (format!("{name}{args}"), None);
    }
    if kept_after < s1 {
        losses.push(rounded_to(kept_after));
    }
    if kept_before < before_point {
        losses.push(format!(
            "a value with more than {kept_before} digits before the point is refused"
        ));
    }
    let to = format!("{name}({},{kept_after})", kept_before + kept_after);
    (
        to.clone(),
        Some(format!(
            "{label}'s {name} holds {holds}, so `numeric{args}` is {to} — {}",
            losses.join(", and ")
        )),
    )
}

/// What SQLite's NUMERIC loses of `numeric(p,s)`, if anything.
///
/// SQLite keeps a NUMERIC value as a 64-bit integer when it is a whole number
/// that fits one — any 18 digits do — and otherwise as an 8-byte float, of
/// which "only the first 15 significant decimal digits of the number are
/// preserved" ("Datatypes In SQLite": Type Affinity). Enforcing no precision
/// or scale is SQLite's convention, as its TEXT enforces no length, and is not
/// reported; losing digits is.
fn sqlite_numeric_loss(declared: Option<(i64, i64)>) -> Option<String> {
    let exact = match declared {
        Some((p, s)) if s <= 0 => p - s <= 18,
        Some((p, _)) => p <= 15,
        None => false,
    };
    (!exact).then(|| {
        "SQLite stores a NUMERIC value as a 64-bit integer when it is a whole number that fits one, and \
         otherwise as a floating-point number that keeps 15 significant digits — so a value with more \
         digits than that is rounded"
            .to_string()
    })
}

/// MySQL's fractional-seconds precision for a PostgreSQL time or timestamp
/// declared with `args`. MySQL's default is whole seconds where PostgreSQL's
/// is microseconds, so a bare TIME or DATETIME rounded every value to the
/// second ("Fractional Seconds in Time Values"); both top out at 6.
fn mysql_fsp(args: &str) -> String {
    if args.is_empty() {
        "(6)".to_string()
    } else {
        args.to_string()
    }
}

/// A column default, read from libpg_query's tree — its shape, not its
/// spelling, which PostgreSQL decorates with casts no other engine reads.
#[derive(Debug, PartialEq)]
enum PgDefault {
    /// A constant, with any cast around it removed: `'open'::text` means
    /// `'open'`, and `::` is a syntax error everywhere else.
    Literal(Literal),
    /// The transaction's clock: `now()`, `current_timestamp`, `localtimestamp`.
    Now,
    /// `current_date`.
    Today,
    /// The time of day: `current_time`, `localtime`.
    TimeOfDay,
    /// `gen_random_uuid()`, or `uuid_generate_v4()` from `uuid-ossp`.
    RandomUuid,
    /// `nextval('seq')` — the sequence as written.
    NextVal(String),
    /// Anything else — PostgreSQL SQL that dbd does not translate.
    Other,
}

#[derive(Debug, PartialEq)]
enum Literal {
    Number(String),
    Text(String),
    Bool(bool),
    Null,
}

/// What the default expression `expr` is, for a column of PostgreSQL type
/// `pg_type`.
///
/// Parsed, not pattern-matched: a string literal can hold `::` or `(` and a
/// function can be schema-qualified, and the tree settles both.
fn read_default(expr: &str, pg_type: &str) -> PgDefault {
    use pg_query::NodeEnum;
    use pg_query::protobuf::AConst;
    use pg_query::protobuf::SqlValueFunctionOp as Svf;
    use pg_query::protobuf::a_const::Val;

    let Ok(parsed) = pg_query::parse(&format!("SELECT {expr}")) else {
        return PgDefault::Other;
    };
    let Some(NodeEnum::SelectStmt(select)) = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.as_ref())
        .and_then(|n| n.node.as_ref())
    else {
        return PgDefault::Other;
    };
    let Some(NodeEnum::ResTarget(res)) = select.target_list.first().and_then(|n| n.node.as_ref()) else {
        return PgDefault::Other;
    };
    let Some(mut node) = res.val.as_deref() else {
        return PgDefault::Other;
    };

    let base = pg_type.trim().to_lowercase();
    let mut boolean = matches!(base.as_str(), "boolean" | "bool");
    // A cast types a literal for PostgreSQL; the value is what the default
    // means. An array cast is the exception — `'{a,b}'::text[]` is array
    // syntax, not a value another engine can read.
    while let Some(NodeEnum::TypeCast(cast)) = node.node.as_ref() {
        let Some(ty) = &cast.type_name else {
            return PgDefault::Other;
        };
        if !ty.array_bounds.is_empty() {
            return PgDefault::Other;
        }
        let names: Vec<&str> = ty
            .names
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(NodeEnum::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect();
        if names.last().is_some_and(|n| matches!(*n, "bool" | "boolean")) {
            boolean = true;
        }
        let Some(arg) = cast.arg.as_deref() else {
            return PgDefault::Other;
        };
        node = arg;
    }

    match node.node.as_ref() {
        Some(NodeEnum::AConst(c)) if c.isnull => PgDefault::Literal(Literal::Null),
        Some(NodeEnum::AConst(c)) => match &c.val {
            Some(Val::Ival(i)) => PgDefault::Literal(Literal::Number(i.ival.to_string())),
            Some(Val::Fval(f)) => PgDefault::Literal(Literal::Number(f.fval.clone())),
            Some(Val::Boolval(b)) => PgDefault::Literal(Literal::Bool(b.boolval)),
            // A boolean column takes `'t'`, `'yes'`, `'on'` … as well as TRUE.
            Some(Val::Sval(s)) if boolean => match s.sval.trim().to_lowercase().as_str() {
                "t" | "true" | "y" | "yes" | "on" | "1" => PgDefault::Literal(Literal::Bool(true)),
                "f" | "false" | "n" | "no" | "off" | "0" => PgDefault::Literal(Literal::Bool(false)),
                _ => PgDefault::Other,
            },
            Some(Val::Sval(s)) => PgDefault::Literal(Literal::Text(s.sval.clone())),
            _ => PgDefault::Other,
        },
        Some(NodeEnum::SqlvalueFunction(f)) => match f.op() {
            Svf::SvfopCurrentTimestamp
            | Svf::SvfopCurrentTimestampN
            | Svf::SvfopLocaltimestamp
            | Svf::SvfopLocaltimestampN => PgDefault::Now,
            Svf::SvfopCurrentDate => PgDefault::Today,
            Svf::SvfopCurrentTime | Svf::SvfopCurrentTimeN | Svf::SvfopLocaltime | Svf::SvfopLocaltimeN => {
                PgDefault::TimeOfDay
            }
            _ => PgDefault::Other,
        },
        Some(NodeEnum::FuncCall(f)) => {
            let name = f.funcname.last().and_then(|n| match n.node.as_ref() {
                Some(NodeEnum::String(s)) => Some(s.sval.to_lowercase()),
                _ => None,
            });
            match (name.as_deref(), f.args.as_slice()) {
                (Some("now" | "transaction_timestamp"), []) => PgDefault::Now,
                (Some("gen_random_uuid" | "uuid_generate_v4"), []) => PgDefault::RandomUuid,
                // `nextval('app.s')`, or `nextval('app.s'::regclass)` as an
                // introspected default spells it.
                (Some("nextval"), [arg]) => {
                    let mut arg = arg;
                    while let Some(NodeEnum::TypeCast(cast)) = arg.node.as_ref() {
                        let Some(inner) = cast.arg.as_deref() else {
                            return PgDefault::Other;
                        };
                        arg = inner;
                    }
                    match arg.node.as_ref() {
                        Some(NodeEnum::AConst(AConst {
                            val: Some(Val::Sval(s)),
                            ..
                        })) => PgDefault::NextVal(s.sval.clone()),
                        _ => PgDefault::Other,
                    }
                }
                _ => PgDefault::Other,
            }
        }
        _ => PgDefault::Other,
    }
}

/// A string literal in the target's syntax.
///
/// MySQL reads a backslash in a string as an escape, so `'a\b'` — a literal
/// backslash in PostgreSQL — must be doubled there, and a line break can be
/// written as one, keeping a column on its line. SQL Server stores a plain
/// `'…'` in the database's code page, so anything outside ASCII needs `N'…'`.
fn string_literal(s: &str, target: Target) -> String {
    let quoted = s.replace('\'', "''");
    match target {
        Target::MySql => format!(
            "'{}'",
            quoted.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
        ),
        Target::TSql if !s.is_ascii() => format!("N'{quoted}'"),
        Target::TSql | Target::Sqlite => format!("'{quoted}'"),
    }
}

/// A random version-4 UUID in SQLite, which has no UUID function: 128 random
/// bits shaped `xxxxxxxx-xxxx-4xxx-[89ab]xxx-xxxxxxxxxxxx`, the text form
/// `gen_random_uuid()` produces.
const SQLITE_RANDOM_UUID: &str = "(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || \
     substr(lower(hex(randomblob(2))), 2) || '-' || substr('89ab', 1 + (abs(random()) % 4), 1) || \
     substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))))";

/// The `DEFAULT` for column `c` — emitted as `ty` — on the target, or `None`
/// to leave it out. Leaving one out is a loss and is reported: a row that
/// omits the column gets NULL instead, or is refused when it is `NOT NULL`.
///
/// A literal goes across; so do the clock and a random UUID, which every target
/// can make, and `nextval` on SQL Server, which has sequences. Anything else is
/// PostgreSQL SQL that dbd does not translate. `numbered` is the one column
/// the target numbers itself, which takes the place of a `nextval` default
/// when it is this one.
#[allow(clippy::too_many_arguments)]
fn column_default(
    pg: &str,
    e: &Entity,
    c: &ColumnDef,
    ty: &str,
    numbered: Option<&str>,
    script: &Script,
    report: &mut Vec<Downgrade>,
) -> Option<String> {
    let target = script.target;
    let consequence = if c.nullable {
        "a row that omits the column now gets NULL"
    } else {
        "a row that omits the column is now refused, since it is NOT NULL"
    };
    let pg_type = c.data_type.trim().to_lowercase();
    let is_array = pg_type.ends_with("[]");
    let base = pg_type.split('(').next().unwrap_or(&pg_type).trim().to_string();
    let mut lose = |to: &str, reason: String| {
        report.push(Downgrade {
            entity: e.name.clone(),
            column: Some(c.name.clone()),
            from: format!("the default `{pg}`"),
            to: to.to_string(),
            reason,
        });
    };

    let mapped = match read_default(pg, &c.data_type) {
        // An array column is a JSON document here, so the empty array is
        // `[]`; any other array literal is PostgreSQL's own syntax.
        PgDefault::Literal(Literal::Text(t)) if is_array => {
            if t.trim() == "{}" {
                "'[]'".to_string()
            } else {
                lose(
                    "no default",
                    "it is a PostgreSQL array literal, and the column is a JSON document here".to_string(),
                );
                return None;
            }
        }
        PgDefault::Literal(Literal::Number(n)) => n,
        PgDefault::Literal(Literal::Text(t)) => string_literal(&t, target),
        PgDefault::Literal(Literal::Bool(b)) => if b { "1" } else { "0" }.to_string(),
        PgDefault::Literal(Literal::Null) => "NULL".to_string(),
        // The clock on a time-of-day column is the time of day. Written as the
        // timestamp, SQLite stored a date and a time in a column of times, and
        // MySQL refuses CURRENT_TIMESTAMP on a TIME column; an expression
        // default takes CURRENT_TIME there, at the column's own precision.
        // `time`, `timetz`, `time (3) with time zone` — not `timestamp`.
        PgDefault::Now | PgDefault::TimeOfDay if matches!(base.split_whitespace().next(), Some("time" | "timetz")) => {
            match target {
                Target::MySql => match ty.trim().to_uppercase().split_once('(') {
                    Some(("TIME", precision)) => format!("(CURRENT_TIME({precision})"),
                    _ => "(CURRENT_TIME)".to_string(),
                },
                Target::Sqlite => "CURRENT_TIME".to_string(),
                Target::TSql => "CONVERT(time, SYSDATETIME())".to_string(),
            }
        }
        // `now()` on a `date` column is today's date; MySQL refuses
        // CURRENT_TIMESTAMP on a DATE column.
        PgDefault::Now if base != "date" => match target {
            // At the column's own fractional precision: MySQL refuses a
            // DATETIME(6) defaulting to CURRENT_TIMESTAMP — "the same value
            // must be used throughout the column definition" ("Automatic
            // Initialization and Updating for TIMESTAMP and DATETIME").
            Target::MySql => match ty.trim().to_uppercase().split_once('(') {
                Some(("DATETIME" | "TIMESTAMP", precision)) => format!("CURRENT_TIMESTAMP({precision}"),
                _ => "CURRENT_TIMESTAMP".to_string(),
            },
            Target::Sqlite => "CURRENT_TIMESTAMP".to_string(),
            Target::TSql => "SYSDATETIMEOFFSET()".to_string(),
        },
        PgDefault::Now | PgDefault::Today => match target {
            Target::MySql => "(CURRENT_DATE)".to_string(),
            Target::TSql => "CONVERT(date, SYSDATETIME())".to_string(),
            Target::Sqlite => "CURRENT_DATE".to_string(),
        },
        PgDefault::RandomUuid => match target {
            // Both random version-4 UUIDs, as gen_random_uuid() makes.
            Target::TSql => "NEWID()".to_string(),
            Target::Sqlite => SQLITE_RANDOM_UUID.to_string(),
            Target::MySql => {
                lose(
                    "`(UUID())`",
                    "MySQL's UUID() is version 1 — built from the clock and the host, not random — \
                     so new ids can be guessed and reveal when and where they were made"
                        .to_string(),
                );
                "(UUID())".to_string()
            }
        },
        PgDefault::NextVal(written) => {
            let seq = sequence_name(&written, e.schema.as_deref());
            match target {
                Target::TSql if script.sequences.contains(&seq) => {
                    let (schema, bare) = match seq.rsplit_once('.') {
                        Some((s, b)) => (Some(s), b),
                        None => (None, seq.as_str()),
                    };
                    format!("NEXT VALUE FOR {}", qualified(schema, bare, target))
                }
                Target::TSql => {
                    lose(
                        "no default",
                        format!(
                            "it draws from `{seq}`, which is not in the emitted script (outside the scope, \
                             or not part of the project) — {consequence}"
                        ),
                    );
                    return None;
                }
                // The table's own counter stands in for the sequence; `numbering`
                // reported that.
                Target::MySql | Target::Sqlite if numbered == Some(c.name.as_str()) => return None,
                Target::MySql | Target::Sqlite => {
                    let counter = match (numbered, target) {
                        (Some(other), _) => format!("the table's one numbered column is `{other}`"),
                        (None, Target::MySql) => {
                            "MySQL numbers only a column that leads the primary key or a UNIQUE".to_string()
                        }
                        (None, _) => "SQLite numbers only a single-column INTEGER PRIMARY KEY".to_string(),
                    };
                    lose(
                        "no default",
                        format!(
                            "{} has no sequences to draw from, and {counter} — {consequence}",
                            target.label()
                        ),
                    );
                    return None;
                }
            }
        }
        // The time of day in a column that holds something else.
        PgDefault::Other | PgDefault::TimeOfDay => {
            lose(
                "no default",
                format!(
                    "dbd translates types and structure, not expressions, and {} may not have what \
                     it calls — {consequence}",
                    target.label()
                ),
            );
            return None;
        }
    };

    // MySQL takes a default on a TEXT, BLOB or JSON column only as an
    // expression (8.0.13 and later): `('open')`, never `'open'`.
    let ty_base = ty.split('(').next().unwrap_or(ty).trim().to_uppercase();
    let needs_expression = target == Target::MySql
        && matches!(
            ty_base.as_str(),
            "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "JSON"
        );
    if needs_expression && !mapped.starts_with('(') && mapped != "NULL" {
        return Some(format!("({mapped})"));
    }
    Some(mapped)
}
