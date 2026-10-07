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
//! Every target has FOREIGN KEY, CHECK and CREATE INDEX, so they are carried.
//! A CHECK expression or an index predicate is PostgreSQL SQL that dbd does not
//! translate, so it goes across verbatim and is reported, like a view body. An
//! index the target would reject outright — an expression key on SQL Server, a
//! key on an unbounded text column on MySQL or SQL Server — is left out and
//! reported, so the script still applies. So is a foreign key to a table the
//! script does not create — one declared `external:`, or left out by `--scope`.
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
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IndexDef, IndexType, SortOrder, TableConstraint,
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
                out.extend(emit_indexes(e, target, &mut report));
            }
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
}

/// Whether `e` becomes an object in the target — and so needs its schema to
/// exist first.
fn emits_an_object(e: &Entity) -> bool {
    matches!(
        e.entity_type,
        EntityType::Table | EntityType::View | EntityType::MaterializedView
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

    format!("CREATE TABLE {name} (\n{}\n);", table_body(&lines))
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
    let parent_name = match parent_schema {
        Some(s) => format!("{s}.{}", fk.ref_table),
        None => fk.ref_table.clone(),
    };
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
fn emit_indexes(e: &Entity, target: Target, report: &mut Vec<Downgrade>) -> Vec<String> {
    let Some(td) = &e.table_def else { return Vec::new() };
    let bare = e.name.rsplit('.').next().unwrap_or(&e.name);
    let table = qualified(e.schema.as_deref(), bare, target);
    td.indexes
        .iter()
        .filter_map(|idx| emit_index(e, td, idx, bare, &table, target, report))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn emit_index(
    e: &Entity,
    td: &crate::entity::TableDef,
    idx: &IndexDef,
    bare: &str,
    table: &str,
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
