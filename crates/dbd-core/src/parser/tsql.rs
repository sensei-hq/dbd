//! The T-SQL walk — every declaration and every reference, from a token stream.
//!
//! # What this reads, and what it deliberately does not
//!
//! STATEMENT HEADS. `CREATE PROCEDURE [dbo].[sp_X]` declares an object;
//! `FROM [dbo].[Issues]` refers to one. Both are a keyword followed by a
//! qualified name, and that is the whole grammar this needs.
//!
//! It does not read expressions, column lists, control flow or types. A T-SQL
//! change script has no nested scopes, no overloads and no generics — the
//! structure that makes a real parser necessary is absent, and pretending to
//! more than the head would be inventing it. The practical consequence is that
//! a T-SQL entity has **no `table_def`**: dbd can say what exists and what
//! references what, not what columns it has. `reconcile` needs the structure,
//! so it cannot run on T-SQL — the same position SQLite is in, for the same
//! honest reason.
//!
//! # A statement ends where the next one begins
//!
//! T-SQL does not require `;` and most real code omits it. So this does not
//! look for statement ends at all — it scans for statement HEADS and ignores
//! everything between them. A head it does not know is not an error; it is a
//! statement this reader has nothing to say about.
//!
//! # `ALTER` declares, or refers, depending on the object
//!
//! `ALTER PROCEDURE` carries the COMPLETE body — T-SQL's syntax requires it,
//! and the statement replaces the object rather than editing it. The same holds
//! for `ALTER VIEW`, `ALTER FUNCTION` and `ALTER TRIGGER`. A file whose only
//! statement is one of those **declares** the object; its definition is right
//! there.
//!
//! `ALTER TABLE` never carries a definition. It is `ADD`, `DROP` or `ALTER
//! COLUMN` — an edit to a table defined elsewhere.
//!
//! Reading every `ALTER` alike gets one of the two wrong whichever way it goes,
//! and both shapes are common: measured over one corpus, 271 files ship
//! procedures as `ALTER PROCEDURE`, while `ALTER TABLE` outnumbers `CREATE
//! TABLE` 159 to 101.
//!
//! # A qualified call is an edge; a bare one is a built-in
//!
//! T-SQL **requires** a scalar user-defined function to be schema-qualified,
//! and a built-in never is — `GETDATE()`, `ISNULL()`, `LEN()`. So the
//! qualification *is* the distinction, read off the grammar rather than out of
//! a list of known built-in names that would go stale.
//!
//! This is the soft/hard split dbd has to defer to `references::resolve_references`
//! for PostgreSQL, because Postgres does not require the qualification. T-SQL
//! does, so the answer is available here and the reference is emitted hard.
//!
//! Ported from sensei's `indexer::lang::sql::tsql`.

use crate::entity::{Entity, EntityType, REF_TYPE_FUNCTION, Reference};
use crate::parser::FileReferences;
use crate::parser::lex::{self, Tok};

/// What a `CREATE` or `ALTER` head names.
#[derive(Clone, Copy)]
struct Object {
    entity_type: EntityType,
    /// Whether `ALTER` on this kind carries the whole definition — see the
    /// module note.
    alter_defines: bool,
}

/// How one dialect differs from another inside the same walk.
///
/// The walk itself — find a head, read the name after it — is the same for
/// every SQL dialect. What differs is small and specific, and putting it in a
/// struct keeps a second dialect from being a second copy of the walk.
#[derive(Clone, Copy)]
pub(crate) struct WalkRules {
    /// How to tokenise. See [`lex::LexRules`].
    pub lex: lex::LexRules,
    /// Whether `ALTER <object>` can carry the object's whole definition.
    ///
    /// T-SQL's `ALTER PROCEDURE` does and MySQL's does not — MySQL's changes
    /// characteristics only (`COMMENT`, `SQL SECURITY`), and a body change
    /// needs `DROP` then `CREATE`. So the same statement declares in one
    /// dialect and refers in the other.
    pub alter_can_define: bool,
    /// Whether a two-part name's first segment is the DATABASE rather than a
    /// schema.
    ///
    /// MySQL has no schemas: `shop.users` is the `users` table in the `shop`
    /// database. Reading that first part as a schema would put two databases'
    /// tables in one namespace, which is the merge `Entity::catalog` exists to
    /// prevent.
    pub two_part_is_catalog: bool,
    /// Whether a bare function call can be told from a built-in by its
    /// qualification alone.
    ///
    /// T-SQL REQUIRES a scalar UDF to be schema-qualified, so the grammar
    /// answers it. MySQL does not, so a qualified call is still an edge but a
    /// bare one cannot be distinguished from `NOW()` and is left alone.
    pub qualified_call_is_an_edge: bool,
}

/// The object kinds this reader knows, and what each is in dbd's vocabulary.
///
/// `TYPE` and `SYNONYM` are deliberately absent: 3 files each in a 2,421-file
/// corpus, and neither maps to an `EntityType` without inventing one. A reader
/// that declared them as something else would be reporting a kind the source
/// did not write.
fn object_kind(word: &Tok<'_>, rules: &WalkRules) -> Option<Object> {
    let table = [
        ("procedure", EntityType::Procedure, true),
        ("proc", EntityType::Procedure, true),
        ("function", EntityType::Function, true),
        ("trigger", EntityType::Trigger, true),
        ("view", EntityType::View, true),
        ("table", EntityType::Table, false),
    ];
    table.iter().find_map(|&(kw, entity_type, body_carrying)| {
        word.is(kw).then_some(Object {
            entity_type,
            alter_defines: body_carrying && rules.alter_can_define,
        })
    })
}

/// A qualified name the reader has just read.
struct Qualified {
    /// The database, when the source wrote a three-part name.
    catalog: Option<String>,
    /// The schema the source wrote, `None` when it wrote none.
    ///
    /// An `Option` rather than an empty string, and that is the point: "the
    /// source did not qualify this" is a fact a caller acts on — an unqualified
    /// name depends on the connection's default schema, which no file states —
    /// and an empty string is a value a caller cannot tell from a schema
    /// genuinely spelled that way.
    schema: Option<String>,
    object: String,
    /// The token index just past the name.
    next: usize,
}

impl Qualified {
    /// `schema.object`, or just `object` when the source did not qualify it.
    ///
    /// The catalog is kept apart in [`Entity::catalog`] rather than folded in,
    /// so `name` stays the two-part form every other caller reads.
    fn name(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{schema}.{}", self.object),
            None => self.object.clone(),
        }
    }
}

/// Read a qualified name at `i`: `a`, `a.b`, or `a.b.c`.
///
/// Unlike an indexer scanning one codebase, dbd keeps the DATABASE of a
/// three-part name: `OtherDb.dbo.Users` and `dbo.Users` are different objects
/// when a scan spans databases, and dropping the first part would merge them.
/// See [`Entity::catalog`].
fn qualified(toks: &[Tok<'_>], i: usize, rules: &WalkRules) -> Option<Qualified> {
    let mut parts: Vec<String> = Vec::new();
    let mut at = i;
    loop {
        // A VARIABLE or a temp table is never an object name. Stopping here
        // keeps `INSERT INTO #temp` from minting a table called `temp`.
        if matches!(toks.get(at), Some(Tok::Var(_))) {
            return None;
        }
        let name = toks.get(at)?.name()?;
        parts.push(name.to_string());
        at += 1;
        // A dot CONTINUES the name only when a name follows it. `t.*` ends the
        // name at `t`.
        match (toks.get(at), toks.get(at + 1)) {
            (Some(Tok::Punct('.')), Some(next)) if next.name().is_some() => at += 1,
            _ => break,
        }
        if parts.len() >= 3 {
            break;
        }
    }
    let object = parts.pop()?;
    // `a.b` is `schema.object` in T-SQL and `database.object` in MySQL, which
    // has no schemas. Reading MySQL's first part as a schema would put two
    // databases' tables in one namespace.
    let (catalog, schema) = if rules.two_part_is_catalog {
        let catalog = parts.pop();
        // A three-part MySQL name does not exist; if one appears, the leading
        // segment is dropped rather than invented into a level MySQL has not
        // got.
        (catalog, None)
    } else {
        let schema = parts.pop();
        (parts.pop(), schema)
    };
    Some(Qualified {
        catalog,
        schema,
        object,
        next: at,
    })
}

impl WalkRules {
    /// T-SQL: `ALTER PROCEDURE` carries the body, `a.b` is `schema.object`, and
    /// a qualified call is a user-defined function because the language
    /// requires the qualification.
    pub(crate) const TSQL: Self = Self {
        lex: lex::LexRules::TSQL,
        alter_can_define: true,
        two_part_is_catalog: false,
        qualified_call_is_an_edge: true,
    };

    /// MySQL: `ALTER` never carries a body, `a.b` is `database.object` because
    /// MySQL has no schemas, and a bare call cannot be told from `NOW()`
    /// because MySQL does not require a UDF to be qualified.
    pub(crate) const MYSQL: Self = Self {
        lex: lex::LexRules::MYSQL,
        alter_can_define: false,
        two_part_is_catalog: true,
        qualified_call_is_an_edge: true,
    };
}

/// Read one file into the entities it declares and the references they make.
pub(crate) fn read(rules: WalkRules, sql: &str) -> (Vec<Entity>, FileReferences, Signals) {
    let mut entities: Vec<Entity> = Vec::new();
    let mut file_refs = FileReferences::default();
    let mut signals = Signals::default();
    // What the file DROPped or ALTERed, resolved against what it declares only
    // once every batch has been read. `DROP PROCEDURE x` followed by `CREATE
    // PROCEDURE x` is the redeploy idiom and the commonest shape in a release
    // folder — the drop names the very object being declared, so counting it
    // as a change would put nearly every declaration file in `Mixed`.
    // Measured: it did, 54.5% of the corpus.
    let mut changed: Vec<String> = Vec::new();

    for (_line, batch) in lex::batches(sql) {
        let toks = lex::tokens_with(rules.lex, batch);
        walk(&rules, &toks, &mut entities, &mut file_refs, &mut signals, &mut changed);
    }

    signals.changes = changed.iter().any(|name| !entities.iter().any(|e| &e.name == name));
    (entities, file_refs, signals)
}

/// What the statements amounted to, for classifying the file itself.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Signals {
    pub declares: bool,
    pub changes: bool,
    pub data: bool,
}

/// Walk one batch.
fn walk(
    rules: &WalkRules,
    toks: &[Tok<'_>],
    entities: &mut Vec<Entity>,
    file_refs: &mut FileReferences,
    signals: &mut Signals,
    changed: &mut Vec<String>,
) {
    // References belong to the most recent declaration in this batch, and to
    // NOTHING before there is one — which is the whole of a migration script.
    // A T-SQL procedure body runs to the end of its batch, so "most recent"
    // is the right owner rather than a heuristic.
    let mut owner: Option<usize> = None;
    let mut i = 0usize;

    while i < toks.len() {
        let t = &toks[i];

        // `CREATE [OR ALTER] <object> <name>` — a DECLARATION.
        if t.is("create") {
            let mut head = i + 1;
            if toks.get(head).is_some_and(|x| x.is("or")) && toks.get(head + 1).is_some_and(|x| x.is("alter")) {
                head += 2;
            }
            // `CREATE UNIQUE NONCLUSTERED INDEX` — modifiers before the object
            // word. Skipped rather than matched, because the list is
            // open-ended and the object word is what decides.
            while toks
                .get(head)
                .is_some_and(|x| x.is("unique") || x.is("clustered") || x.is("nonclustered") || x.is("columnstore"))
            {
                head += 1;
            }
            if let Some(word) = toks.get(head)
                && let Some(obj) = object_kind(word, rules)
                && let Some(name) = qualified(toks, head + 1, rules)
            {
                owner = Some(declare(entities, &name, obj.entity_type));
                signals.declares = true;
                i = name.next;
                continue;
            }
        }

        // `ALTER <object>` DECLARES when the statement carries the whole
        // definition and REFERS when it is an edit. `DROP` always refers: it
        // names something defined elsewhere.
        if t.is("alter") || t.is("drop") {
            let dropping = t.is("drop");
            let mut head = i + 1;
            if let Some(word) = toks.get(head)
                && let Some(obj) = object_kind(word, rules)
            {
                head += 1;
                // `DROP TABLE IF EXISTS x`
                if toks.get(head).is_some_and(|x| x.is("if")) {
                    head += 1;
                    if toks.get(head).is_some_and(|x| x.is("exists")) {
                        head += 1;
                    }
                }
                if let Some(name) = qualified(toks, head, rules) {
                    if !dropping && obj.alter_defines {
                        owner = Some(declare(entities, &name, obj.entity_type));
                        signals.declares = true;
                    } else {
                        // Recorded, not decided: whether this is a change
                        // depends on what the whole file declares.
                        changed.push(name.name());
                        refer(entities, file_refs, owner, &name, RefKind::Writes);
                    }
                    i = name.next;
                    continue;
                }
            }
        }

        // The reference heads. Each is a keyword whose next token starts a
        // qualified name.
        let kind = if t.is("from") || t.is("join") {
            Some(RefKind::Reads)
        } else if t.is("into") || t.is("update") || t.is("merge") {
            Some(RefKind::Writes)
        } else if t.is("exec") || t.is("execute") {
            Some(RefKind::Calls)
        } else if t.is("references") {
            // A FOREIGN KEY names the table it points at.
            Some(RefKind::Reads)
        } else {
            None
        };
        if let Some(kind) = kind {
            if t.is("insert") || t.is("into") || t.is("update") || t.is("merge") || t.is("delete") {
                signals.data = true;
            }
            // `DELETE FROM x` and `INSERT INTO x` are reached through their own
            // keyword, so `FROM`/`INTO` alone carries them. A SUBQUERY opens
            // with `FROM (`, which names no object.
            if let Some(name) = qualified(toks, i + 1, rules) {
                refer(entities, file_refs, owner, &name, kind);
                i = name.next;
                continue;
            }
        }
        if t.is("insert") || t.is("delete") {
            signals.data = true;
        }

        // A QUALIFIED CALL IN AN EXPRESSION — `SELECT dbo.fnIssues(@x)`. See
        // the module note: the qualification is what distinguishes a
        // user-defined function from a built-in, because T-SQL requires it.
        if rules.qualified_call_is_an_edge
            && t.name().is_some()
            && matches!(toks.get(i + 1), Some(Tok::Punct('.')))
            && let Some(name) = qualified(toks, i, rules)
            && name.schema.is_some()
            && matches!(toks.get(name.next), Some(Tok::Punct('(')))
        {
            let next = name.next;
            refer(entities, file_refs, owner, &name, RefKind::Calls);
            i = next;
            continue;
        }
        i += 1;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RefKind {
    Reads,
    Writes,
    Calls,
}

/// Record a declaration, returning its index in `entities`.
///
/// A redeploy script's `DROP … CREATE` names the object twice, and a release
/// folder ships the same procedure repeatedly. Declaring it once keeps a caller
/// from seeing two nodes for one object.
fn declare(entities: &mut Vec<Entity>, name: &Qualified, entity_type: EntityType) -> usize {
    let full = name.name();
    if let Some(existing) = entities
        .iter()
        .position(|e| e.name == full && e.catalog == name.catalog && e.entity_type == entity_type)
    {
        return existing;
    }
    let mut entity = Entity::new(entity_type, &full);
    entity.catalog = name.catalog.clone();
    entities.push(entity);
    entities.len() - 1
}

/// Attribute a reference to the declaration that made it, or to the file.
///
/// A reference made before any declaration in the batch belongs to no entity.
/// Attaching it to whatever happens to be declared next would fabricate an
/// edge, so it is not attached to an entity at all — it goes to
/// [`FileReferences`], because the file is what made it.
///
/// This used to drop such a reference. Over a 2,154-file T-SQL corpus that was
/// 20,929 of 43,754 references (47.8%), and two-thirds of the loss was pure
/// data scripts whose references are the whole point of the file (#21).
fn refer(
    entities: &mut [Entity],
    file_refs: &mut FileReferences,
    owner: Option<usize>,
    name: &Qualified,
    kind: RefKind,
) {
    let full = match &name.catalog {
        Some(catalog) => format!("{catalog}.{}", name.name()),
        None => name.name(),
    };
    let Some(owner) = owner else {
        // No declaration to own it: the file made this reference.
        push_unique(
            match kind {
                RefKind::Reads => &mut file_refs.reads,
                RefKind::Writes => &mut file_refs.writes,
                RefKind::Calls => &mut file_refs.calls,
            },
            &full,
        );
        return;
    };
    let entity = &mut entities[owner];
    if entity.name == full {
        // A procedure that reads itself is recursion, not a dependency.
        return;
    }

    match kind {
        RefKind::Reads => push_unique(&mut entity.reads, &full),
        RefKind::Writes => push_unique(&mut entity.writes, &full),
        RefKind::Calls => {}
    }
    let ref_type = (kind == RefKind::Calls).then(|| REF_TYPE_FUNCTION.to_string());
    if !entity
        .references
        .iter()
        .any(|r| r.name == full && r.ref_type == ref_type)
    {
        entity.references.push(Reference {
            name: full.clone(),
            ref_type,
            // This walk never invents a schema: an unqualified name is reported
            // unqualified rather than guessed at, so whatever is here is the
            // source's own.
            schema_source: crate::entity::SchemaSource::Stated,
        });
    }
    push_unique(&mut entity.refers, &full);
}

fn push_unique(v: &mut Vec<String>, item: &str) {
    if !v.iter().any(|x| x == item) {
        v.push(item.to_string());
    }
}
