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
//! translate, so it goes across verbatim and is reported, like a view body. An
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
//! skipped so the omission is visible.

use crate::entity::{
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IdentityKind, IndexDef, IndexType, SortOrder, TableConstraint,
    TableDef,
};
use crate::error::{DbdError, Result};
use crate::parser::Dialect;
use std::collections::HashSet;

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
        externals: design
            .entities()
            .iter()
            .filter(|e| e.entity_type == EntityType::External)
            .map(|e| e.name.clone())
            .collect(),
        sequences: entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Sequence && e.errors.is_empty())
            .map(|e| e.name.clone())
            .collect(),
    };
    let mut out: Vec<String> = Vec::new();
    let mut report = Vec::new();

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

    for e in entities.iter().filter(|e| e.errors.is_empty()) {
        match e.entity_type {
            EntityType::Table => {
                out.push(emit_table(e, &script, &mut report));
                out.extend(emit_indexes(e, &script, &mut report));
                out.extend(descriptions(e, target));
            }
            EntityType::View | EntityType::MaterializedView => {
                if let Some(sql) = emit_view(e, target, &mut report) {
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

    Ok((join_script(&header, &out, target), report))
}

/// What the script as a whole holds. A statement naming something outside it —
/// a key to a table it does not create — cannot apply on its own.
struct Script {
    target: Target,
    /// `schema.name` of every table the script creates.
    tables: HashSet<String>,
    /// `schema.name` of every table the design declares `external:`.
    externals: HashSet<String>,
    /// `schema.name` of every sequence in the emitted set. Only SQL Server
    /// creates them; a default drawing from one outside it cannot apply.
    sequences: HashSet<String>,
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
    let numbered = numbered_column(td, &pk, target, e.schema.as_deref());
    let bounded = bounded_key_columns(e, td, script);

    let mut lines: Vec<String> = Vec::new();
    for c in &td.columns {
        let before = report.len();
        let mut ty = map_type(&c.data_type, target, e, c, report);
        if bounded.contains(&c.name) {
            let to = key_bound(&c.data_type, target);
            report.push(Downgrade {
                entity: e.name.clone(),
                column: Some(c.name.clone()),
                from: format!("`{}` in a key", c.data_type),
                to: to.to_string(),
                reason: format!(
                    "{} — so the column is bounded to {to} instead, which keeps the key; a longer value \
                     is now refused",
                    unindexable(&c.data_type, target).unwrap_or("the target cannot key it as it is")
                ),
            });
            ty = to.to_string();
        }
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
            col.push_str(&format!(" {clause}"));
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
    if !inline_pk.is_empty() && !key_in_place {
        lines.push(format!("  PRIMARY KEY ({}),", quote_all(&inline_pk, target)));
    }
    for c in td.columns.iter().filter(|c| c.is_unique && !c.is_pk) {
        lines.push(format!("  UNIQUE ({}),", target.quote(&c.name)));
    }

    for con in &td.constraints {
        match con {
            // Skipped when the columns already declared one inline — a table
            // with two PRIMARY KEY clauses is rejected by every target.
            TableConstraint::PrimaryKey { columns, .. }
                if !columns.is_empty() && inline_pk.is_empty() && !key_in_place =>
            {
                lines.push(format!("  PRIMARY KEY ({}),", quote_all(columns, target)));
            }
            TableConstraint::Unique { columns, .. } if !columns.is_empty() => {
                lines.push(format!("  UNIQUE ({}),", quote_all(columns, target)));
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

    // CHECK exists everywhere, but its expression is PostgreSQL SQL that dbd
    // does not translate — so it goes across verbatim and is reported.
    for con in &td.constraints {
        if let TableConstraint::Check { name, expression } = con {
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
            let named = name
                .as_ref()
                .map(|n| format!("CONSTRAINT {} ", target.quote(n)))
                .unwrap_or_default();
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

/// Whether `column` leads one of the table's keys or indexes.
fn leads_a_key(td: &TableDef, pk: &[String], column: &str) -> bool {
    let leads = |cols: &[String]| cols.first().is_some_and(|c| c == column);
    leads(pk)
        || td.columns.iter().any(|c| c.name == column && c.is_unique)
        || td.constraints.iter().any(|con| match con {
            TableConstraint::Unique { columns, .. } => leads(columns),
            _ => false,
        })
        || td
            .indexes
            .iter()
            .any(|ix| ix.columns.first().is_some_and(|k| !k.is_expression && k.name == column))
}

/// The one column the target numbers itself, if any.
///
/// Every target numbers at most one column per table, and two of them only
/// some columns: MySQL one that leads a key (InnoDB keeps the counter in that
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
                "MySQL numbers only a column that leads a key, which this one does not — inserts must \
                 now supply it"
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

/// The columns of `e` that sit in a key the target refuses on their emitted
/// type — unbounded text or binary — and so must be bounded.
///
/// A key is the primary key, a UNIQUE constraint or index, or a foreign key
/// the script keeps. Bounding keeps more than the alternative: leaving the
/// constraint out would lose the key and every foreign key that points at it,
/// where a bound only refuses a value longer than it. A foreign key's own
/// columns take the same bound as the key they point at, so the two sides
/// still match. An ordinary index is not a reason to change a column's type;
/// one on unbounded text is left out instead ([`emit_index`]).
fn bounded_key_columns(e: &Entity, td: &TableDef, script: &Script) -> HashSet<String> {
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
    td.columns
        .iter()
        .filter(|c| keyed.contains(c.name.as_str()) && unindexable(&c.data_type, script.target).is_some())
        .map(|c| c.name.clone())
        .collect()
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

/// Why the target cannot index a column of PostgreSQL type `pg` as it is
/// emitted — the type maps to an unbounded text or binary column.
fn unindexable(pg: &str, target: Target) -> Option<&'static str> {
    let t = pg.trim().to_lowercase();
    let base = t.split('(').next().unwrap_or(&t).trim();
    let unbounded = t.ends_with("[]") || matches!(base, "text" | "json" | "jsonb" | "bytea");
    match target {
        Target::MySql if unbounded => Some("MySQL cannot index a TEXT, BLOB or JSON column without a prefix length"),
        Target::TSql if unbounded => Some("SQL Server cannot index an nvarchar(max) or varbinary(max) column"),
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
    let bounded = bounded_key_columns(e, td, script);
    td.indexes
        .iter()
        .filter_map(|idx| emit_index(e, td, idx, bare, &table, &bounded, target, report))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn emit_index(
    e: &Entity,
    td: &crate::entity::TableDef,
    idx: &IndexDef,
    bare: &str,
    table: &str,
    bounded: &HashSet<String>,
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
            .find(|c| c.name == col.name)
            .map(|c| c.data_type.as_str())
            // A column bounded for a key is no longer unbounded.
            && !bounded.contains(&col.name)
            && let Some(why) = unindexable(ty, target)
        {
            blockers.push(format!("{why} (`{}` is {ty})", col.name));
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

fn emit_view(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> Option<String> {
    let lost = |from: String, to: &str, reason: String| Downgrade {
        entity: e.name.clone(),
        column: None,
        from,
        to: to.to_string(),
        reason,
    };
    let Some(body) = e.body.first() else {
        let d = lost(
            format!("a {} whose body dbd could not read", e.entity_type.tag()),
            "nothing",
            "there is no SELECT to emit, so it is missing from the script".to_string(),
        );
        let note = d.comment(target);
        report.push(d);
        return Some(note);
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
    report.push(lost(
        "a view body in PostgreSQL SQL".to_string(),
        "the same text, untranslated",
        "dbd translates types and structure, not expressions — anything PostgreSQL-specific inside \
         the SELECT has to be checked by hand"
            .to_string(),
    ));
    let mut out: Vec<String> = report[before..].iter().map(|d| d.comment(target)).collect();
    out.push(format!("CREATE VIEW {name} AS {body};"));
    Some(out.join("\n"))
}

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
        // `now()` on a `date` column is today's date; MySQL refuses
        // CURRENT_TIMESTAMP on a DATE column.
        PgDefault::Now if base != "date" => match target {
            Target::MySql | Target::Sqlite => "CURRENT_TIMESTAMP".to_string(),
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
                        (None, Target::MySql) => "MySQL numbers only a column that leads a key".to_string(),
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
        PgDefault::Other => {
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
    let needs_expression = target == Target::MySql && matches!(ty_base.as_str(), "TEXT" | "BLOB" | "JSON");
    if needs_expression && !mapped.starts_with('(') && mapped != "NULL" {
        return Some(format!("({mapped})"));
    }
    Some(mapped)
}
