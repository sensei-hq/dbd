use crate::config::RoleEntry;
use crate::entity::{Entity, EntityType};

/// Supabase-managed schemas that must never be dropped — includes `public`, which
/// Supabase exposes via PostgREST (on a plain `postgres` target `public` is an
/// ordinary project schema and is droppable with `--schemas`).
/// Supabase schemas the project owns **nothing** in.
///
/// Supabase creates and maintains these, and everything inside them is its —
/// so `reconcile --prune` must never drop their contents, however they came to
/// be in `managed_schemas`.
///
/// Deliberately **excludes `public`**: see [`SUPABASE_PROTECTED`].
pub const SUPABASE_INFRASTRUCTURE: &[&str] = &[
    "auth",
    "storage",
    "realtime",
    "graphql_public",
    "supabase_functions",
    "pgbouncer",
    "pgsodium",
    "vault",
    "extensions",
    "supabase_migrations",
];

/// Schemas protected from `DROP SCHEMA` on a `supabase` target: the
/// infrastructure set **plus `public`**.
///
/// `public` is the reason this is a second list rather than the same one.
/// Dropping and recreating it loses the grants and policies PostgREST needs,
/// so the *schema object* is protected — but its **contents are the
/// project's**, which is why `reset` still drops entities in it and
/// [`prune_is_forbidden_in`] still allows pruning it.
///
/// On a plain `postgres` target `public` is an ordinary project schema and is
/// droppable with `--schemas`.
pub const SUPABASE_PROTECTED: &[&str] = &[
    "public",
    "auth",
    "storage",
    "realtime",
    "graphql_public",
    "supabase_functions",
    "pgbouncer",
    "pgsodium",
    "vault",
    "extensions",
    "supabase_migrations",
];

/// Generate DDL SQL from an entity.
///
/// For schema/extension/role: generates CREATE statements.
/// For file-based entities (table, view, etc.): reads the DDL file, decoded as
/// the project scan decodes it (see [`crate::source_text`]). Reading it with
/// the stdlib instead turned a UTF-16 file the scan had loaded into `None` —
/// which `apply` and `combine` take to mean "no DDL", skipping the entity.
pub fn ddl_from_entity(entity: &Entity) -> Option<String> {
    match entity.entity_type {
        EntityType::Schema => Some(format!("CREATE SCHEMA IF NOT EXISTS \"{}\";", entity.name)),
        EntityType::Extension => {
            let schema = entity.schema.as_deref().unwrap_or("public");
            Some(format!(
                "CREATE EXTENSION IF NOT EXISTS \"{}\" WITH SCHEMA \"{}\";",
                entity.name, schema
            ))
        }
        EntityType::Role => Some(generate_role_script(entity)),
        EntityType::External => None,
        _ => entity
            .file
            .as_ref()
            .and_then(|f| crate::source_text::read_to_string(f).ok()),
    }
}

/// The DDL that runs for `entity` on PostgreSQL: [`ddl_from_entity`], opened
/// with the search path the parser resolved its unqualified names against when
/// that path is the project's — `source.search_path`, for a file that states
/// none of its own.
///
/// The parser read a pathless `create table a` in `ddl/table/app/` against
/// `source.search_path: [app]` as `app.a`, and `inspect` said so; but the file
/// ran as written, on whatever path the session had, so the table landed in
/// `public` and `diff` reported `+ create app.a` forever. The path is now a
/// statement ahead of the file rather than an edit inside it: the file's text
/// is never rewritten, and a file that states its own path (which wins over
/// the project's) gets nothing added.
///
/// The one place both `apply` (the Postgres adapter) and `combine` take a
/// file's runnable DDL from, so the script `combine` writes runs as `apply`
/// does — and `reconcile` and `deploy`, which apply through the adapter, with
/// them.
pub fn runnable_ddl(entity: &Entity) -> Option<String> {
    let ddl = ddl_from_entity(entity)?;
    Some(match project_search_path(entity) {
        Some(set) => format!("{set}\n{ddl}"),
        None => ddl,
    })
}

/// The text a migration or hook script runs as on PostgreSQL: the script,
/// opened with `path` when it states no path of its own.
///
/// - A script that states its own path (`SET search_path …`) runs as written.
/// - Otherwise it runs on `path`, `public` kept on it as [`runnable_ddl`] keeps
///   it: for a hook, `source.search_path`; for an entity's migration file, the
///   path that entity's CREATE runs on (the one its DDL file sets before
///   it, else `source.search_path`).
/// - With no `path`, it runs on the session's default (`RESET search_path`).
///
/// Migration and hook scripts were sent bare, so they ran on whatever path the
/// last DDL file had set on the connection: a `data.sql` naming `notes` failed
/// with `relation "notes" does not exist` in a project whose bare names
/// resolve on `app`, and a hook's tables were judged on one path and run on
/// another. A hook's scope analysis reads its bare names on this same path
/// (see `design::hooks::derive_dependencies`).
pub fn runnable_script(sql: &str, path: Option<&[crate::entity::PathEntry]>) -> String {
    if crate::parser::pg::common::extract_search_paths_via_pg_query(sql).stated() {
        return sql.to_string();
    }
    let set = match path {
        Some(entries) if !entries.is_empty() => search_path_statement(entries),
        _ => "RESET search_path;".to_string(),
    };
    format!("{set}\n{sql}")
}

/// `SET search_path TO …;` for an entity whose names were resolved against
/// `source.search_path`, or `None` when the file stated its own path or the
/// session default applies. Only a file-backed entity has names to resolve:
/// what dbd generates (schemas, extensions, roles) is fully qualified.
pub(crate) fn project_search_path(entity: &Entity) -> Option<String> {
    let path = &entity.schema_path;
    if path.source != crate::entity::PathSource::Project || path.entries.is_empty() || entity.file.is_none() {
        return None;
    }
    Some(search_path_statement(&path.entries))
}

/// `SET search_path TO <entries>, "public";` — `public` appended unless the
/// path already names it, as [`keep_public_on_search_path`] does for a path a
/// file states, so an extension installed there stays reachable.
///
/// Every entry is quoted: an unquoted name is folded to lower case, which would
/// point a mixed-case schema somewhere else. `"$user"` is Postgres's own
/// spelling of the role placeholder and is written that way.
fn search_path_statement(entries: &[crate::entity::PathEntry]) -> String {
    use crate::entity::PathEntry;
    let names: Vec<String> = entries
        .iter()
        .map(|e| match e {
            PathEntry::Schema(s) => s.clone(),
            PathEntry::CurrentUser => "$user".to_string(),
        })
        .collect();
    set_search_path_with_public(&names, false)
}

/// One statement of a migration script, as the enum-value handling sees it.
enum MigrationStatement {
    /// `ALTER TYPE … ADD VALUE`, deparsed with `IF NOT EXISTS` — and whether
    /// the script already said so.
    AddValue {
        idempotent: String,
        as_written: bool,
    },
    /// `SET search_path …` or `RESET search_path`: the context of what follows.
    SearchPath,
    Other,
}

/// Classify one statement `piece`, by its parse tree.
fn migration_statement(piece: &str) -> MigrationStatement {
    use pg_query::NodeEnum;
    let Ok(mut parsed) = pg_query::parse(piece) else {
        return MigrationStatement::Other;
    };
    let [stmt] = parsed.protobuf.stmts.as_mut_slice() else {
        return MigrationStatement::Other;
    };
    match stmt.stmt.as_mut().and_then(|n| n.node.as_mut()) {
        // `RENAME VALUE` is the same node with `old_val` set.
        Some(NodeEnum::AlterEnumStmt(alter)) if alter.old_val.is_empty() && !alter.new_val.is_empty() => {
            let as_written = alter.skip_if_new_val_exists;
            alter.skip_if_new_val_exists = true;
            match pg_query::deparse(&parsed.protobuf) {
                Ok(idempotent) => MigrationStatement::AddValue { idempotent, as_written },
                Err(_) => MigrationStatement::Other,
            }
        }
        Some(NodeEnum::VariableSetStmt(set)) if set.name.eq_ignore_ascii_case("search_path") => {
            MigrationStatement::SearchPath
        }
        _ => MigrationStatement::Other,
    }
}

/// The `ALTER TYPE … ADD VALUE` statements of migration script `sql`, each as
/// `ADD VALUE IF NOT EXISTS`, with the `SET search_path` statements among them
/// kept in place so each names the type it named in the script. `None` when
/// the script adds no enum value, or the grammar cannot split it.
///
/// What an apply runs and commits ahead of the rest of a version: Postgres
/// will not use an enum value inside the transaction that added it ("unsafe
/// use of new value"), and the rest of the version may — a default, a CHECK,
/// a data script. `IF NOT EXISTS` because a run that fails after this commit
/// leaves the value in place, and the next run adds it again.
pub fn enum_value_additions(sql: &str) -> Option<String> {
    let pieces = pg_query::split_with_parser(sql).ok()?;
    let mut kept: Vec<String> = Vec::new();
    let mut adds = false;
    for piece in pieces {
        match migration_statement(piece) {
            MigrationStatement::AddValue { idempotent, .. } => {
                adds = true;
                kept.push(idempotent);
            }
            MigrationStatement::SearchPath => kept.push(piece.trim().to_string()),
            MigrationStatement::Other => {}
        }
    }
    adds.then(|| format!("{};\n", kept.join(";\n")))
}

/// Migration script `sql` with each `ALTER TYPE … ADD VALUE` as `ADD VALUE IF
/// NOT EXISTS`, and every other byte as written.
///
/// What runs as the script itself once [`enum_value_additions`] has committed
/// its values: the additions are then no-ops, and the script's other
/// statements run where the script put them. Also what lets a run that
/// creates an enum from the design's DDL — which already has every value —
/// go on to a later version's `ADD VALUE` of one of them.
pub fn idempotent_enum_value_additions(sql: &str) -> String {
    let Ok(pieces) = pg_query::split_with_parser(sql) else {
        return sql.to_string();
    };
    let mut out = String::with_capacity(sql.len());
    let mut copied = 0;
    for piece in pieces {
        let MigrationStatement::AddValue {
            idempotent,
            as_written: false,
        } = migration_statement(piece)
        else {
            continue;
        };
        // `split_with_parser` returns slices of `sql`, and a slice opens with
        // whatever comments precede its statement: those are kept, and the
        // statement is replaced from its first token on.
        let start = piece.as_ptr() as usize - sql.as_ptr() as usize + first_token_at(piece);
        out.push_str(&sql[copied..start]);
        out.push_str(&idempotent);
        copied = piece.as_ptr() as usize - sql.as_ptr() as usize + piece.len();
    }
    out.push_str(&sql[copied..]);
    out
}

/// The byte offset in `piece` of its first token that is not a comment — where
/// its statement begins. 0 when the scanner cannot read it.
fn first_token_at(piece: &str) -> usize {
    use pg_query::protobuf::Token;
    let comment = |t: i32| t == Token::SqlComment as i32 || t == Token::CComment as i32;
    pg_query::scan(piece)
        .ok()
        .and_then(|scan| scan.tokens.into_iter().find(|t| !comment(t.token)))
        .and_then(|t| usize::try_from(t.start).ok())
        .unwrap_or(0)
}

/// Whether statement `piece` is an `ALTER TYPE … ADD VALUE`.
pub(crate) fn is_enum_value_addition(piece: &str) -> bool {
    matches!(migration_statement(piece), MigrationStatement::AddValue { .. })
}

/// `SET [LOCAL] search_path TO <names>, "public";` — `public` added unless
/// `names` already holds it.
///
/// Each name is one schema, exactly as `SET` reads its arguments, so it is
/// written as a quoted identifier: unquoted, a mixed-case name would be folded
/// to lower case and point somewhere else. The empty name — Supabase's `''`,
/// "no schema" — has no identifier spelling (`""` is an error) and is written
/// as the string `''`, which `SET` reads as the same empty name.
fn set_search_path_with_public(names: &[String], local: bool) -> String {
    let mut written: Vec<String> = names
        .iter()
        .map(|name| match name.as_str() {
            "" => "''".to_string(),
            name => crate::sql_quote::ident(name),
        })
        .collect();
    if !names.iter().any(|n| n == crate::reconcile::DEFAULT_SCHEMA) {
        written.push(crate::sql_quote::ident(crate::reconcile::DEFAULT_SCHEMA));
    }
    let scope = if local { "LOCAL " } else { "" };
    format!("SET {scope}search_path TO {};", written.join(", "))
}

/// `sql` with `public` kept on every search path one of its own top-level `SET
/// search_path` statements sets: DDL files often set the path to their own
/// schemas (`sensei, extensions`) and leave `public` off it, which hides the
/// extensions installed there.
///
/// No text of `sql` is changed. After each such statement whose path lacks
/// `public`, a statement of its own sets the same path with `public` added.
/// This replaced a regex that rewrote `set search_path to …;` wherever the text
/// appeared, up to the next `;`: a Supabase-style `SECURITY DEFINER` function's
/// `SET search_path TO ''` clause became `'' as $$ select 1, public;` and failed
/// with `column "public" does not exist`, a body `return 'g; ok'` after such a
/// clause deployed returning `'g, public; ok'`, and a string literal quoting
/// the statement was altered. So the statements are found in libpg_query's
/// parse tree — their boundaries and node types — and only a top-level
/// `VariableSetStmt` for `search_path` counts. A function's own `SET` clause,
/// a body, a literal, a comment and a `set_config(…)` call are never one.
///
/// A statement whose path is reset to the default (`RESET`, `TO DEFAULT`)
/// gets nothing: the default already holds `public`. Nor does one followed at
/// once by another `SET search_path`, which replaces its path before anything
/// runs on it — and which makes this idempotent: what it adds is followed by
/// nothing that needs it. A script libpg_query cannot parse whole (syntax
/// newer than its grammar) is split by its scanner instead and each piece
/// parsed alone; one it cannot even scan is returned as it is, for the server
/// to report.
///
/// Shared by `apply` (the Postgres adapter runs every script through it) and
/// `combine`, so the script `combine` writes runs the way `apply` does.
pub(crate) fn keep_public_on_search_path(sql: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    // A fast path, not a decision: a script that never spells the setting's
    // name sets no search path, and parsing every large data script to learn
    // that is waste. One that does is parsed, and the tree decides.
    if !sql.to_ascii_lowercase().contains("search_path") {
        return Cow::Borrowed(sql);
    }
    let Some(statements) = top_level_statements(sql) else {
        return Cow::Borrowed(sql);
    };
    let mut out = String::with_capacity(sql.len() + 64);
    let mut copied = 0;
    for (i, statement) in statements.iter().enumerate() {
        let Some(SearchPathSet { names, local }) = &statement.sets_search_path else {
            continue;
        };
        let replaced_at_once = statements
            .get(i + 1)
            .is_some_and(|next| next.sets_search_path.is_some() || next.resets_search_path);
        if replaced_at_once || names.iter().any(|n| n == crate::reconcile::DEFAULT_SCHEMA) {
            continue;
        }
        let added = set_search_path_with_public(names, *local);
        match statement.terminator {
            Some(semicolon) => {
                out.push_str(&sql[copied..=semicolon]);
                out.push('\n');
                out.push_str(&added);
                copied = semicolon + 1;
            }
            // The last statement, with no `;`: one is written on a line of its
            // own, so a trailing `--` comment cannot swallow it.
            None => {
                out.push_str(&sql[copied..]);
                out.push_str("\n;\n");
                out.push_str(&added);
                copied = sql.len();
            }
        }
    }
    if copied == 0 {
        return Cow::Borrowed(sql);
    }
    out.push_str(&sql[copied..]);
    Cow::Owned(out)
}

/// A top-level statement, as [`keep_public_on_search_path`] needs it.
struct TopLevel {
    /// The byte offset of the `;` ending it, or `None` when it runs to the end
    /// of the script unterminated.
    terminator: Option<usize>,
    /// The path a `SET [LOCAL] search_path TO …` sets.
    sets_search_path: Option<SearchPathSet>,
    /// `RESET search_path` or `SET search_path TO DEFAULT`.
    resets_search_path: bool,
}

struct SearchPathSet {
    names: Vec<String>,
    local: bool,
}

/// `sql`'s top-level statements in order: from libpg_query's parse when the
/// whole script parses, otherwise from its scanner's split with each piece
/// parsed alone. `None` when the scanner cannot read the script either.
fn top_level_statements(sql: &str) -> Option<Vec<TopLevel>> {
    if let Ok(parsed) = pg_query::parse(sql) {
        return Some(
            parsed
                .protobuf
                .stmts
                .iter()
                .map(|raw| {
                    let start = usize::try_from(raw.stmt_location).unwrap_or(0);
                    // `stmt_len` 0 means "to the end of the script"; otherwise
                    // the statement ends where its `;` is.
                    let terminator = match usize::try_from(raw.stmt_len).unwrap_or(0) {
                        0 => None,
                        len => Some(start + len).filter(|&at| sql.as_bytes().get(at) == Some(&b';')),
                    };
                    let node = raw.stmt.as_ref().and_then(|s| s.node.as_ref());
                    classify(node, terminator)
                })
                .collect(),
        );
    }
    let pieces = pg_query::split_with_scanner(sql).ok()?;
    Some(
        pieces
            .iter()
            .map(|piece| {
                // Each piece is a slice of `sql`; its offset is where it starts.
                let end = piece.as_ptr() as usize - sql.as_ptr() as usize + piece.len();
                let terminator = (sql.as_bytes().get(end) == Some(&b';')).then_some(end);
                let parsed = pg_query::parse(piece).ok();
                let node = parsed
                    .as_ref()
                    .and_then(|p| p.protobuf.stmts.first())
                    .and_then(|raw| raw.stmt.as_ref())
                    .and_then(|s| s.node.as_ref());
                classify(node, terminator)
            })
            .collect(),
    )
}

/// What a top-level statement does to the search path, read off its node.
fn classify(node: Option<&pg_query::NodeEnum>, terminator: Option<usize>) -> TopLevel {
    use pg_query::protobuf::VariableSetKind;
    let mut statement = TopLevel {
        terminator,
        sets_search_path: None,
        resets_search_path: false,
    };
    let Some(pg_query::NodeEnum::VariableSetStmt(set)) = node else {
        return statement;
    };
    if !set.name.eq_ignore_ascii_case("search_path") {
        return statement;
    }
    match set.kind() {
        VariableSetKind::VarSetValue => {
            // Every argument of `SET search_path` is a name — an identifier or
            // a string. Anything else is a path dbd cannot restate, so it is
            // left to stand as written.
            let names: Option<Vec<String>> = set.args.iter().map(set_argument).collect();
            statement.sets_search_path = names.map(|names| SearchPathSet {
                names,
                local: set.is_local,
            });
        }
        VariableSetKind::VarSetDefault | VariableSetKind::VarReset => statement.resets_search_path = true,
        _ => {}
    }
    statement
}

/// One `SET` argument's name: the string an identifier or a literal holds.
fn set_argument(node: &pg_query::protobuf::Node) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    match node.node.as_ref()? {
        pg_query::NodeEnum::AConst(c) => match c.val.as_ref()? {
            Val::Sval(s) => Some(s.sval.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The DDL `combine` writes for `entity` in a project whose SQL is `dialect`.
///
/// The authored files are already in the project's dialect and are copied as
/// they are. What dbd generates itself — schemas, extensions, roles — is
/// written by [`ddl_from_entity`] in PostgreSQL, and was emitted that way into
/// every project's script: a T-SQL, MySQL or SQLite script opened with
/// `CREATE SCHEMA IF NOT EXISTS "x";`, which none of those engines accepts.
///
/// - SQL Server creates a schema with no `IF NOT EXISTS`, and only alone in its
///   batch — so the idempotent form wraps it in `EXEC`, and a `GO` ends the
///   batch before the next file's statements.
/// - MySQL's "schema" is a database and SQLite has none, so neither gets a
///   schema statement (as `emit` treats them, and as the SQLite adapter skips
///   the schema on apply).
/// - Extensions and roles exist only in PostgreSQL; another engine's script
///   says so in a comment rather than carrying PostgreSQL SQL, or nothing.
/// - On SQL Server every entity's DDL ends its batch with `GO`, not only the
///   schema's: `CREATE VIEW`, `CREATE PROCEDURE`, `CREATE FUNCTION` and
///   `CREATE TRIGGER` must each be the first statement in a batch, and with the
///   files written back to back a view shared its batch with the table before
///   it — sqlcmd refused the script. A file that already ends in `GO` gets no
///   second one.
///
/// Unrecognised and unstated dialects are read as PostgreSQL by the parser, so
/// they are written as PostgreSQL here too.
pub fn combined_ddl(entity: &Entity, dialect: crate::parser::Dialect) -> Option<String> {
    let ddl = generated_or_authored_ddl(entity, dialect)?;
    Some(match dialect {
        crate::parser::Dialect::TSql if !ends_its_batch(&ddl) => format!("{}\nGO", ddl.trim_end()),
        _ => ddl,
    })
}

/// Whether T-SQL `ddl` already closes its batch: its last line holds only `GO`
/// (sqlcmd reads the separator case-insensitively).
fn ends_its_batch(ddl: &str) -> bool {
    ddl.trim_end()
        .lines()
        .next_back()
        .is_some_and(|last| last.trim().eq_ignore_ascii_case("go"))
}

/// [`combined_ddl`] before SQL Server's batch separator is added.
fn generated_or_authored_ddl(entity: &Entity, dialect: crate::parser::Dialect) -> Option<String> {
    use crate::parser::Dialect;
    match (dialect, entity.entity_type) {
        (Dialect::PostgreSql | Dialect::Unstated, _) => {
            runnable_ddl(entity).map(|ddl| keep_public_on_search_path(&ddl).into_owned())
        }
        (Dialect::TSql, EntityType::Schema) => {
            // `]` doubles inside a bracketed name, then `'` inside the N'…' literal.
            let bracketed = format!("[{}]", entity.name.replace(']', "]]"));
            Some(format!(
                "IF SCHEMA_ID(N'{}') IS NULL EXEC(N'CREATE SCHEMA {}');\nGO",
                entity.name.replace('\'', "''"),
                bracketed.replace('\'', "''")
            ))
        }
        (Dialect::MySql | Dialect::Sqlite, EntityType::Schema) => None,
        (_, EntityType::Extension | EntityType::Role) => Some(format!(
            "-- {} {} is PostgreSQL-only, so this {} script does not create it",
            if entity.entity_type == EntityType::Extension {
                "extension"
            } else {
                "role"
            },
            entity.name.replace('\n', " "),
            dialect.as_label()
        )),
        _ => ddl_from_entity(entity),
    }
}

/// Generate an idempotent role creation script.
///
/// Uses a DO block to check pg_catalog.pg_roles before creating.
/// Grants the roles this one is a member of.
fn generate_role_script(entity: &Entity) -> String {
    let name = &entity.name;
    let mut script = format!(
        "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{name}') THEN\n    CREATE ROLE \"{name}\";\n  END IF;\nEND $$;\n"
    );
    for granted_role in entity.refs_of(crate::entity::RefKind::Member).map(|r| &r.name) {
        script.push_str(&format!("GRANT \"{granted_role}\" TO \"{name}\";\n"));
    }
    script
}

/// Schemas never dropped by reset on any target: Postgres internals plus
/// dbd's own bookkeeping schema (`dbd`).
const ALWAYS_PROTECTED: &[&str] = &["pg_catalog", "information_schema", "pg_toast", "dbd"];

/// Whether `reconcile --prune` must refuse to drop anything in `schema`.
///
/// Prune is the only dbd operation that drops an object the design does not
/// declare, so it is the only one that can destroy something the project never
/// knew about. `reset`, by contrast, emits a `DROP` per *declared* entity and
/// needs no such guard.
///
/// Forbidden: the true system schemas and dbd's own bookkeeping on every
/// target, plus Supabase's [infrastructure][SUPABASE_INFRASTRUCTURE] on a
/// `supabase` target.
///
/// **`public` is allowed**, on both targets. It is protected from `DROP
/// SCHEMA` on Supabase because recreating it loses grants, but the tables in
/// it are the project's and pruning them is the point.
pub fn prune_is_forbidden_in(schema: &str, target: &str) -> bool {
    if ALWAYS_PROTECTED.contains(&schema) {
        return true;
    }
    target == "supabase" && SUPABASE_INFRASTRUCTURE.contains(&schema)
}

/// Schema of an entity, defaulting to `public` when unqualified.
fn entity_schema(entity: &Entity) -> &str {
    entity.schema.as_deref().unwrap_or("public")
}

/// Single-quote-escape a string for embedding in a SQL literal.
fn sql_quote_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Escape a SQL identifier for embedding inside double quotes (doubles any `"`).
/// Identifiers here come from the project's own validated design, but quoting
/// defensively keeps a stray `"` from breaking out of the `DROP … "x"` statement.
fn quote_ident(s: &str) -> String {
    s.replace('"', "\"\"")
}

/// Whether a schema is protected from `DROP SCHEMA` for the given target.
///
/// Always protects the true system schemas. On a `supabase` target, also
/// protects the full Supabase-managed set (including `public`). On `postgres`,
/// `public` is a normal project schema and is dropped.
pub(crate) fn schema_is_protected(schema: &str, target: &str) -> bool {
    if ALWAYS_PROTECTED.contains(&schema) {
        return true;
    }
    if target == "supabase" && SUPABASE_PROTECTED.contains(&schema) {
        return true;
    }
    false
}

/// Emit a `DROP ROUTINE` DO-block dropping every overload of `schema.name`
/// (functions and procedures alike, PG ≥ 11), scoped to that exact schema+name.
fn function_drop_block(schema: &str, name: &str) -> String {
    let s = sql_quote_escape(schema);
    let n = sql_quote_escape(name);
    format!(
        "DO $$ DECLARE r record; BEGIN\n  \
         FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p\n           \
         JOIN pg_namespace n ON n.oid = p.pronamespace\n           \
         WHERE n.nspname = '{s}' AND p.proname = '{n}'\n  \
         LOOP EXECUTE format('DROP ROUTINE IF EXISTS %s CASCADE', r.sig); END LOOP;\nEND $$;"
    )
}

/// Build an entity-level reset script.
///
/// Drops the project's own managed objects individually, in reverse dependency
/// order (functions/procedures → views → tables → sequences → enums). Tables,
/// views, enums and sequences use `DROP … IF EXISTS "schema"."name" CASCADE`
/// (schema defaults to `public`). Functions/procedures use a per-`schema.name`
/// `DO $$ … DROP ROUTINE … $$` block covering every overload.
///
/// `drop_schemas` additionally emits `DROP SCHEMA IF EXISTS "s" CASCADE;` for
/// each schema in `schemas`, silently skipping protected ones (always the true
/// system schemas; on a `supabase` target the full Supabase-managed set incl.
/// `public`). `drop_extensions` additionally emits `DROP EXTENSION IF EXISTS
/// "e" CASCADE;` for each name in `extensions`. Never errors on protected
/// schemas — it skips them.
///
/// Roles are dropped last (postgres target only). Before that, every grant in
/// `grants` (schema → role → perms, as [`build_grants_script`] grants them)
/// held by a role being dropped is revoked on each schema that survives the
/// reset — default privileges included — because Postgres refuses to drop a
/// role that still holds any. A schema this script drops takes its grants with
/// it. Anything else the role holds still blocks the drop, and the error names
/// the role and what it holds.
#[allow(clippy::too_many_arguments)]
pub fn build_reset_script(
    entities: &[&Entity],
    roles: &[RoleEntry],
    extensions: &[String],
    target: &str,
    drop_schemas: bool,
    drop_extensions: bool,
    schemas: &[String],
    grants: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
) -> Result<Option<String>, String> {
    build_reset_script_keeping(
        entities,
        roles,
        extensions,
        target,
        drop_schemas,
        drop_extensions,
        schemas,
        grants,
        &[],
    )
}

/// [`build_reset_script`], keeping the roles named in `keep`: their grants in
/// this database are revoked like every other role's, but they are not
/// dropped.
///
/// A role is server-wide, and one this design declares may hold privileges or
/// objects in another database on the same server — one built from the same
/// design. Dropping it there breaks that database; trying failed this reset.
/// The caller decides which to keep from the server's catalogs (see
/// [`crate::adapter::DatabaseAdapter::roles_held_elsewhere`]). Revoking a kept
/// role's grants here still matters: they are this database's, and left in
/// place they would keep the role "held elsewhere" from the other database's
/// own reset, so no reset would ever drop it.
#[allow(clippy::too_many_arguments)]
pub fn build_reset_script_keeping(
    entities: &[&Entity],
    roles: &[RoleEntry],
    extensions: &[String],
    target: &str,
    drop_schemas: bool,
    drop_extensions: bool,
    schemas: &[String],
    grants: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    keep: &[String],
) -> Result<Option<String>, String> {
    let mut lines = entity_drop_lines(entities);

    // ── Optional schema DROPs (filtered by protected rules) ───
    let mut dropped_schemas = std::collections::HashSet::new();
    if drop_schemas {
        for schema in schemas {
            if schema_is_protected(schema, target) {
                continue;
            }
            lines.push(format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE;", quote_ident(schema)));
            dropped_schemas.insert(schema.as_str());
        }
    }

    // ── Optional extension DROPs ──────────────────────────────
    if drop_extensions {
        for ext in extensions {
            lines.push(format!("DROP EXTENSION IF EXISTS \"{}\" CASCADE;", quote_ident(ext)));
        }
    }

    // ── Role DROPs (postgres only, reverse order) ─────────────
    if target == "postgres" {
        lines.extend(revoke_grant_lines(roles, grants, &dropped_schemas));
        for role in roles.iter().rev().filter(|r| !keep.contains(&r.name)) {
            lines.push(role_drop_block(&role.name));
        }
    }

    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(lines.join("\n")))
}

/// The reset script for a subset scope: the scope's own objects, dropped
/// without `CASCADE`, all or nothing.
///
/// `CASCADE` reaches past the scope: dropping an in-scope enum took the column
/// of an out-of-scope table that used it, with its data. So nothing here
/// cascades — not the objects, schemas or extensions — and Postgres itself
/// decides what depends on what, columns, defaults and objects dbd never made
/// included. Without `CASCADE` the drops must run dependents first, so they
/// follow `entities` (apply order) in reverse rather than grouping by kind:
/// a view calling an in-scope function goes before it. Extensions go before
/// schemas, which may hold them.
///
/// The drops share one block: if any is refused because something outside the
/// scope depends on it, every drop rolls back and the error names the object
/// and its dependents (Postgres's `DETAIL`, which the driver's message leaves
/// out), each by its schema. Roles are a full reset's alone and never appear
/// here.
pub fn build_scoped_reset_script(
    entities: &[&Entity],
    extensions: &[String],
    target: &str,
    drop_schemas: bool,
    drop_extensions: bool,
    schemas: &[String],
) -> Option<String> {
    let mut drops = Vec::new();
    let mut seen_routines = std::collections::HashSet::new();
    for e in entities.iter().rev() {
        let schema = entity_schema(e);
        let (_, bare) = crate::entity::split_qualified_name(&e.name);
        let qualified = format!("\"{}\".\"{}\"", quote_ident(schema), quote_ident(&bare));
        match e.entity_type {
            EntityType::Function | EntityType::Procedure => {
                if seen_routines.insert((schema.to_string(), bare.clone())) {
                    drops.push(format!(
                        "FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p\n    \
                         JOIN pg_namespace n ON n.oid = p.pronamespace\n    \
                         WHERE n.nspname = '{}' AND p.proname = '{}'\n  \
                         LOOP EXECUTE format('DROP ROUTINE IF EXISTS %s', r.sig); END LOOP;",
                        sql_quote_escape(schema),
                        sql_quote_escape(&bare)
                    ));
                }
            }
            EntityType::MaterializedView => drops.push(format!("DROP MATERIALIZED VIEW IF EXISTS {qualified};")),
            EntityType::View => drops.push(format!("DROP VIEW IF EXISTS {qualified};")),
            EntityType::Table => drops.push(format!("DROP TABLE IF EXISTS {qualified};")),
            EntityType::Sequence => drops.push(format!("DROP SEQUENCE IF EXISTS {qualified};")),
            EntityType::Enum => drops.push(format!("DROP TYPE IF EXISTS {qualified};")),
            _ => {}
        }
    }
    if drop_extensions {
        for ext in extensions {
            drops.push(format!("DROP EXTENSION IF EXISTS \"{}\";", quote_ident(ext)));
        }
    }
    if drop_schemas {
        for schema in schemas.iter().filter(|s| !schema_is_protected(s, target)) {
            drops.push(format!("DROP SCHEMA IF EXISTS \"{}\";", quote_ident(schema)));
        }
    }
    if drops.is_empty() {
        return None;
    }
    // An empty search_path, local to the block's transaction, so Postgres
    // names every object in the refusal by its schema: it qualifies only what
    // the session's path does not reach, and a pooled session's path is
    // whatever the last DDL file `SET`.
    Some(format!(
        "DO $dbd_reset$ DECLARE r record; what text; held text; BEGIN\n  \
         PERFORM set_config('search_path', '', true);\n  {}\n\
         EXCEPTION WHEN dependent_objects_still_exist THEN\n  \
         GET STACKED DIAGNOSTICS what = MESSAGE_TEXT, held = PG_EXCEPTION_DETAIL;\n  \
         RAISE EXCEPTION 'scoped reset refused, nothing was dropped — %: %. A scoped reset drops only \
         what its scope holds, and never cascades: add the dependents to the scope, or reset without --scope.', \
         what, replace(held, E'\\n', '; ') USING ERRCODE = 'dependent_objects_still_exist';\n\
         END $dbd_reset$;",
        drops.join("\n  ")
    ))
}

/// The reset script for a SQLite database: `DROP VIEW` and `DROP TABLE IF
/// EXISTS "name";` for each of the project's views and tables, `entities`
/// given in apply order and dropped in reverse.
///
/// SQLite has no schemas, extensions, roles, sequences or enums, and no
/// `CASCADE` — the PostgreSQL script failed on its first `"public"."items"
/// CASCADE`. Its drop of a table a foreign key still references fails too, and
/// SQLite reads no reference out of a verbatim DDL file to order by, so the
/// drops run in one transaction with every foreign key deferred to its
/// commit (`PRAGMA defer_foreign_keys`, which the commit itself resets), by
/// when the tables that hold the keys are gone as well. An index or trigger
/// goes with its table.
pub fn build_sqlite_reset_script(entities: &[&Entity]) -> Option<String> {
    let drops: Vec<String> = entities
        .iter()
        .rev()
        .filter_map(|e| {
            let kind = match e.entity_type {
                EntityType::View => "VIEW",
                EntityType::Table => "TABLE",
                _ => return None,
            };
            // A SQLite name is bare; a schema-qualified one names `main`'s
            // table the adapter strips it to.
            let (_, bare) = crate::entity::split_qualified_name(&e.name);
            Some(format!("DROP {kind} IF EXISTS {};", crate::sql_quote::ident(&bare)))
        })
        .collect();
    if drops.is_empty() {
        return None;
    }
    Some(format!(
        "BEGIN;\nPRAGMA defer_foreign_keys = ON;\n{}\nCOMMIT;",
        drops.join("\n")
    ))
}

/// The privileges a grant entry's perms confer, split the way
/// [`build_grants_script`] grants them: `usage` is schema USAGE, and everything
/// else is a table privilege, upper-cased and joined. Shared so a revoke is the
/// exact inverse of the grant it undoes.
fn grant_privileges(perms: &[String]) -> (bool, Option<String>) {
    let usage = perms.iter().any(|p| p == "usage");
    let table: Vec<String> = perms
        .iter()
        .filter(|p| *p != "usage")
        .map(|p| p.to_uppercase())
        .collect();
    (usage, (!table.is_empty()).then(|| table.join(", ")))
}

/// One guarded block per (surviving schema, dropped role) revoking what
/// [`build_grants_script`] granted there: schema USAGE, the table privileges on
/// every table in it, and the `ALTER DEFAULT PRIVILEGES` entry.
///
/// Guarded on both names existing, because REVOKE errors on a missing schema or
/// role and a reset must run against a database apply never reached. Schemas
/// and roles are sorted so the script is byte-stable run to run.
///
/// The default-privileges revoke undoes the entry of the role running the
/// reset — the one `apply` set when run as the same user. An entry another user
/// set is not this script's to touch; it still blocks the drop, and
/// [`role_drop_block`] says so.
fn revoke_grant_lines(
    roles: &[RoleEntry],
    grants: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    dropped_schemas: &std::collections::HashSet<&str>,
) -> Vec<String> {
    let dropped_roles: std::collections::HashSet<&str> = roles.iter().map(|r| r.name.as_str()).collect();
    let mut schema_names: Vec<&String> = grants
        .keys()
        .filter(|s| !dropped_schemas.contains(s.as_str()))
        .collect();
    schema_names.sort();

    let mut lines = Vec::new();
    for schema in schema_names {
        let mut role_names: Vec<&String> = grants[schema]
            .keys()
            .filter(|r| dropped_roles.contains(r.as_str()))
            .collect();
        role_names.sort();
        for role in role_names {
            let (usage, table) = grant_privileges(&grants[schema][role]);
            let (s, r) = (quote_ident(schema), quote_ident(role));
            let mut revokes = Vec::new();
            if usage {
                revokes.push(format!("REVOKE USAGE ON SCHEMA \"{s}\" FROM \"{r}\";"));
            }
            if let Some(privs) = table {
                revokes.push(format!("REVOKE {privs} ON ALL TABLES IN SCHEMA \"{s}\" FROM \"{r}\";"));
                revokes.push(format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"{s}\" REVOKE {privs} ON TABLES FROM \"{r}\";"
                ));
            }
            if revokes.is_empty() {
                continue;
            }
            lines.push(format!(
                "DO $$ BEGIN\n  \
                 IF EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = '{}')\n     \
                 AND EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{}') THEN\n    \
                 {}\n  \
                 END IF;\nEND $$;",
                sql_quote_escape(schema),
                sql_quote_escape(role),
                revokes.join("\n    ")
            ));
        }
    }
    lines
}

/// `DROP ROLE IF EXISTS`, re-raising Postgres's refusal with the reason in the
/// message.
///
/// A role that still holds a privilege or owns an object cannot be dropped, and
/// Postgres says so as `role "x" cannot be dropped because some objects depend
/// on it` — the *what* is only in the error's DETAIL, which the driver's
/// message leaves out. The refusal stands (reset revokes only the schema grants
/// design.yaml declares, and never `DROP OWNED BY`, which would drop objects
/// the project does not own); this only makes it say which role and why.
///
/// It does not say *who* granted what is left: it said "privileges dbd did not
/// grant", which was false for the grants dbd made in another database, and is
/// for any a hook or policy file made. What reset can say is that it does not
/// revoke them.
fn role_drop_block(role: &str) -> String {
    format!(
        "DO $$ DECLARE held text; BEGIN\n  \
         DROP ROLE IF EXISTS \"{}\";\n\
         EXCEPTION WHEN dependent_objects_still_exist THEN\n  \
         GET STACKED DIAGNOSTICS held = PG_EXCEPTION_DETAIL;\n  \
         RAISE EXCEPTION 'reset cannot drop role \"%\" — it still holds privileges or owns objects \
         reset does not revoke (it revokes only the schema grants design.yaml declares): %. \
         Revoke or reassign them, then re-run reset.', '{}', \
         replace(held, E'\\n', '; ') USING ERRCODE = 'dependent_objects_still_exist';\n\
         END $$;",
        quote_ident(role),
        sql_quote_escape(role)
    )
}

/// Entity DROPs in reverse dependency order:
/// functions/procedures → materialized views → views → tables → sequences →
/// enums. Materialized views come before plain views/tables because a matview
/// reads them. Function/procedure overloads share a name, so they collapse to
/// one DO-block per (schema, name).
fn entity_drop_lines(entities: &[&Entity]) -> Vec<String> {
    let mut funcs = Vec::new();
    let mut matviews = Vec::new();
    let mut views = Vec::new();
    let mut tables = Vec::new();
    let mut sequences = Vec::new();
    let mut enums = Vec::new();
    for e in entities {
        match e.entity_type {
            EntityType::Function | EntityType::Procedure => funcs.push(*e),
            EntityType::MaterializedView => matviews.push(*e),
            EntityType::View => views.push(*e),
            EntityType::Table => tables.push(*e),
            EntityType::Sequence => sequences.push(*e),
            EntityType::Enum => enums.push(*e),
            _ => {}
        }
    }

    let mut lines = Vec::new();
    let mut seen_funcs = std::collections::HashSet::new();
    for e in &funcs {
        let schema = entity_schema(e);
        let (_, bare) = crate::entity::split_qualified_name(&e.name);
        if seen_funcs.insert((schema.to_string(), bare.clone())) {
            lines.push(function_drop_block(schema, &bare));
        }
    }
    for e in &matviews {
        lines.push(drop_object("MATERIALIZED VIEW", e));
    }
    for e in &views {
        lines.push(drop_object("VIEW", e));
    }
    for e in &tables {
        lines.push(drop_object("TABLE", e));
    }
    for e in &sequences {
        lines.push(drop_object("SEQUENCE", e));
    }
    for e in &enums {
        lines.push(drop_object("TYPE", e));
    }
    lines
}

/// `DROP <KIND> IF EXISTS "schema"."name" CASCADE;` using the entity's bare name.
fn drop_object(kind: &str, entity: &Entity) -> String {
    let schema = quote_ident(entity_schema(entity));
    let (_, bare) = crate::entity::split_qualified_name(&entity.name);
    let bare = quote_ident(&bare);
    format!("DROP {kind} IF EXISTS \"{schema}\".\"{bare}\" CASCADE;")
}

/// Build a grants script for Supabase PostgREST schemas.
pub fn build_grants_script(
    schema_grants: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    supabase_schemas: &[String],
) -> Option<String> {
    if schema_grants.is_empty() && supabase_schemas.is_empty() {
        return None;
    }

    let mut lines = Vec::new();

    // Supabase schema grants
    for schema in supabase_schemas {
        for role in &["anon", "authenticated", "service_role"] {
            lines.push(format!("GRANT USAGE ON SCHEMA \"{}\" TO \"{}\";", schema, role));
        }
    }

    // Per-schema grants from config
    for (schema, role_perms) in schema_grants {
        for (role, perms) in role_perms {
            let (usage, table_perms) = grant_privileges(perms);
            if usage {
                lines.push(format!("GRANT USAGE ON SCHEMA \"{}\" TO \"{}\";", schema, role));
            }
            if let Some(perms_str) = table_perms {
                lines.push(format!(
                    "GRANT {} ON ALL TABLES IN SCHEMA \"{}\" TO \"{}\";",
                    perms_str, schema, role
                ));
                lines.push(format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"{}\" GRANT {} ON TABLES TO \"{}\";",
                    schema, perms_str, role
                ));
            }
        }
    }

    if lines.is_empty() {
        return None;
    }

    // Notify PostgREST to reload
    lines.push("NOTIFY pgrst, 'reload config';".to_string());

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A migration or hook script that states no path runs on the one the
    /// project gives bare names — `source.search_path`, with `public` kept on
    /// it as for a DDL file — and one that states its own runs as written.
    #[test]
    fn a_script_stating_no_path_runs_on_the_projects() {
        let app = [crate::entity::PathEntry::Schema("app".to_string())];
        let sql = "update notes set tag = 'old';\n";
        assert_eq!(
            runnable_script(sql, Some(&app)),
            format!("SET search_path TO \"app\", \"public\";\n{sql}")
        );
        let own = "set search_path to core;\nupdate zones set id = id;\n";
        assert_eq!(runnable_script(own, Some(&app)), own);
    }

    /// With no project path, a script stating none runs on the session's
    /// default — not on whatever path the last DDL file left the connection.
    #[test]
    fn a_script_in_a_project_without_a_path_runs_on_the_session_default() {
        let sql = "insert into items values (1);\n";
        assert_eq!(runnable_script(sql, None), format!("RESET search_path;\n{sql}"));
    }

    /// The additions a run commits ahead of a version: each `ADD VALUE`
    /// made idempotent, with the path that names its type — and nothing else
    /// of the script, which runs later in the version's own transaction.
    #[test]
    fn a_scripts_enum_value_additions_keep_their_search_path_and_nothing_else() {
        let sql = "set search_path to app;\n\
                   alter type mood add value 'archived' after 'ok';\n\
                   alter type mood rename value 'bad' to 'poor';\n\
                   alter table people alter column mood set default 'archived';\n";
        let additions = enum_value_additions(sql).expect("the script adds a value");
        assert!(
            additions.to_lowercase().contains("set search_path to app"),
            "{additions}"
        );
        assert!(
            additions.contains("ADD VALUE IF NOT EXISTS 'archived' AFTER 'ok'"),
            "{additions}"
        );
        assert!(!additions.to_lowercase().contains("rename value"), "{additions}");
        assert!(!additions.to_lowercase().contains("set default"), "{additions}");
    }

    #[test]
    fn a_script_adding_no_enum_value_has_no_additions() {
        assert!(enum_value_additions("alter table t add column c int;").is_none());
        assert!(enum_value_additions("alter type mood rename value 'a' to 'b';").is_none());
        assert!(enum_value_additions("not sql at all ;;").is_none());
    }

    /// Run after its values committed, a script's `ADD VALUE` must be a
    /// no-op — and the rest of the file must reach the server exactly as
    /// written, comments and spelling included.
    #[test]
    fn only_the_add_value_statements_of_a_script_are_rewritten() {
        let sql = "-- keep me\nALTER TYPE app.mood ADD VALUE 'archived';\nUPDATE app.t SET c = 'x;y' /* ; */ ;\n";
        assert_eq!(
            idempotent_enum_value_additions(sql),
            "-- keep me\nALTER TYPE app.mood ADD VALUE IF NOT EXISTS 'archived';\nUPDATE app.t SET c = 'x;y' /* ; */ ;\n"
        );
        let already = "ALTER TYPE app.mood ADD VALUE IF NOT EXISTS 'archived';\n";
        assert_eq!(idempotent_enum_value_additions(already), already);
        let unparsable = "ALTER TYPE app.mood ADD VALUE 'x'; this is not sql";
        assert_eq!(idempotent_enum_value_additions(unparsable), unparsable);
    }

    /// The path each statement of `sql` runs on, in order: the args of the
    /// last top-level `SET search_path` before it, read from libpg_query's
    /// tree so a spelling change cannot pass or fail it.
    fn paths_by_statement(sql: &str) -> Vec<Vec<String>> {
        let parsed = pg_query::parse(sql).unwrap_or_else(|e| panic!("{e}:\n{sql}"));
        let mut path = Vec::new();
        let mut out = Vec::new();
        for raw in &parsed.protobuf.stmts {
            if let Some(pg_query::NodeEnum::VariableSetStmt(set)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref())
                && set.name == "search_path"
            {
                path = set.args.iter().filter_map(set_argument).collect();
                continue;
            }
            out.push(path.clone());
        }
        out
    }

    /// `SET LOCAL` keeps its scope, the empty path `''` is restated as a
    /// string (an empty identifier is an error), and a path that is replaced
    /// by the next statement, or already holds `public`, gets nothing.
    #[test]
    fn public_follows_each_path_a_statement_sets_in_its_own_spelling() {
        let sql = "set local search_path to app;\nselect 1;\n\
                   set search_path = '';\nselect 2;\n\
                   set search_path to a;\nset search_path to b, public;\nselect 3;\n\
                   set search_path to \"Mixed\", \"$user\";\nselect 4;\n\
                   reset search_path;\nselect 5;\n";
        let out = keep_public_on_search_path(sql);
        assert!(out.contains("set local search_path to app;\nSET LOCAL search_path TO \"app\", \"public\";"));
        let paths = paths_by_statement(&out);
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(paths[0], strings(&["app", "public"]), "{out}");
        assert_eq!(paths[1], strings(&["", "public"]), "{out}");
        assert_eq!(paths[2], strings(&["b", "public"]), "{out}");
        assert_eq!(paths[3], strings(&["Mixed", "$user", "public"]), "{out}");
        assert_eq!(
            out.matches("SET search_path TO").count(),
            2,
            "only where a path lacks it:\n{out}"
        );
    }

    /// What is added is followed by nothing that needs it, so running a script
    /// through twice — `combine`'s output applied by `apply` — adds nothing more.
    #[test]
    fn keeping_public_twice_adds_nothing_the_second_time() {
        let sql = "set search_path to app; -- the app's own\ncreate table t (id int);\nset search_path to x";
        let once = keep_public_on_search_path(sql).into_owned();
        assert_eq!(keep_public_on_search_path(&once), once);
        assert_eq!(
            paths_by_statement(&once)[0],
            vec!["app".to_string(), "public".to_string()]
        );
        assert!(
            once.ends_with("set search_path to x\n;\nSET search_path TO \"x\", \"public\";"),
            "an unterminated last statement gets its `;` on a line of its own:\n{once}"
        );
    }

    /// A script libpg_query's grammar refuses whole — syntax newer than it, or
    /// an error further on — is still split by its scanner, and its `SET
    /// search_path` statements still found; one it cannot scan is left alone.
    #[test]
    fn an_unparsable_script_still_has_its_set_statements_found() {
        let sql = "set search_path to app;\nthis is not sql;\nselect 'set search_path to y;';\n";
        let out = keep_public_on_search_path(sql);
        assert_eq!(
            out,
            "set search_path to app;\nSET search_path TO \"app\", \"public\";\nthis is not sql;\nselect 'set search_path to y;';\n"
        );
        let unscannable = "set search_path to app;\nselect 'never closed";
        assert_eq!(keep_public_on_search_path(unscannable), unscannable);
    }

    #[test]
    fn dbd_schema_is_protected_on_all_targets() {
        assert!(schema_is_protected("dbd", "postgres"));
        assert!(schema_is_protected("dbd", "supabase"));
    }

    #[test]
    fn ddl_for_schema() {
        let entity = Entity::new(EntityType::Schema, "config");
        let ddl = ddl_from_entity(&entity).unwrap();
        assert_eq!(ddl, "CREATE SCHEMA IF NOT EXISTS \"config\";");
    }

    #[test]
    fn ddl_for_extension() {
        let mut entity = Entity::new(EntityType::Extension, "uuid-ossp");
        entity.schema = Some("extensions".to_string());
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("uuid-ossp"));
        assert!(ddl.contains("extensions"));
    }

    #[test]
    fn ddl_for_role_without_grants() {
        let entity = Entity::new(EntityType::Role, "basic");
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("CREATE ROLE"));
        assert!(ddl.contains("pg_roles"));
        assert!(!ddl.contains("GRANT"));
    }

    #[test]
    fn ddl_for_role_with_grants() {
        let mut entity = Entity::new(EntityType::Role, "advanced");
        entity.refs = vec![crate::entity::Ref::stated("basic", crate::entity::RefKind::Member)];
        let ddl = ddl_from_entity(&entity).unwrap();
        assert!(ddl.contains("CREATE ROLE"));
        assert!(ddl.contains("GRANT \"basic\" TO \"advanced\""));
    }

    #[test]
    fn ddl_for_external_returns_none() {
        let entity = Entity::new(EntityType::External, "auth.users");
        assert!(ddl_from_entity(&entity).is_none());
    }

    // ── reset entity-set builders ─────────────────────────
    fn ent(ty: EntityType, name: &str) -> Entity {
        Entity::new(ty, name)
    }

    /// A representative project: public.users (table), config.lookups (table),
    /// a view, an enum, a sequence and a function.
    fn sample_entities() -> Vec<Entity> {
        vec![
            ent(EntityType::Table, "public.users"),
            ent(EntityType::Table, "config.lookups"),
            ent(EntityType::View, "config.genders"),
            ent(EntityType::Enum, "config.status"),
            ent(EntityType::Sequence, "config.counter"),
            ent(EntityType::Function, "config.touch"),
        ]
    }

    fn refs(entities: &[Entity]) -> Vec<&Entity> {
        entities.iter().collect()
    }

    #[test]
    fn reset_default_drops_entities_not_schemas() {
        let entities = sample_entities();
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(script.contains("DROP TABLE IF EXISTS \"config\".\"lookups\" CASCADE"));
        assert!(script.contains("DROP VIEW IF EXISTS \"config\".\"genders\" CASCADE"));
        assert!(script.contains("DROP TYPE IF EXISTS \"config\".\"status\" CASCADE"));
        assert!(script.contains("DROP SEQUENCE IF EXISTS \"config\".\"counter\" CASCADE"));
        // Function uses a DROP ROUTINE DO-block scoped to schema+name.
        assert!(script.contains("DO $$"));
        assert!(script.contains("DROP ROUTINE IF EXISTS"));
        assert!(script.contains("n.nspname = 'config' AND p.proname = 'touch'"));
        // No schema or extension drops on the default path; never errors on public.
        assert!(!script.contains("DROP SCHEMA"));
        assert!(!script.contains("DROP EXTENSION"));
    }

    #[test]
    fn reset_default_supabase_drops_public_entities_without_error() {
        // Regression: the old builder returned Err on `public`, aborting the
        // whole reset. On the default path (no schema drops) a supabase project
        // with public.users must drop the table and never error.
        let entities = sample_entities();
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "supabase",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA"));
    }

    #[test]
    fn reset_default_reverse_dependency_order() {
        let entities = sample_entities();
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        let func = script.find("DROP ROUTINE").unwrap();
        let view = script.find("DROP VIEW").unwrap();
        let table = script.find("DROP TABLE").unwrap();
        let seq = script.find("DROP SEQUENCE").unwrap();
        let enm = script.find("DROP TYPE").unwrap();
        // functions/procedures → views → tables → sequences → enums
        assert!(func < view, "functions before views");
        assert!(view < table, "views before tables");
        assert!(table < seq, "tables before sequences");
        assert!(seq < enm, "sequences before enums");
    }

    #[test]
    fn reset_drops_materialized_view_before_its_deps() {
        let mut entities = sample_entities();
        // A matview reads config.genders (a view) — it must be dropped first.
        entities.push(ent(EntityType::MaterializedView, "config.genders_mv"));
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(
            script.contains("DROP MATERIALIZED VIEW IF EXISTS \"config\".\"genders_mv\" CASCADE"),
            "reset must drop the materialized view; got:\n{script}"
        );
        // `find("DROP VIEW")` matches only the plain view — "DROP MATERIALIZED
        // VIEW" has no "DROP VIEW" substring.
        let mv = script.find("DROP MATERIALIZED VIEW").unwrap();
        let view = script.find("DROP VIEW").unwrap();
        let table = script.find("DROP TABLE").unwrap();
        assert!(mv < view, "materialized view dropped before plain views");
        assert!(mv < table, "materialized view dropped before tables");
    }

    #[test]
    fn reset_function_overloads_single_block() {
        // Two Function entities sharing schema.name collapse to one DO-block.
        let entities = vec![
            ent(EntityType::Function, "config.touch"),
            ent(EntityType::Function, "config.touch"),
        ];
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(script.matches("DO $$").count(), 1);
    }

    #[test]
    fn reset_drop_schemas_postgres_drops_public() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "public".to_string()];
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            true,
            false,
            &schemas,
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"public\" CASCADE"));
    }

    #[test]
    fn reset_drop_schemas_supabase_keeps_public_and_auth() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "public".to_string(), "auth".to_string()];
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "supabase",
            true,
            false,
            &schemas,
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"public\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"auth\" CASCADE"));
    }

    #[test]
    fn reset_drop_schemas_always_excludes_system() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string(), "pg_catalog".to_string()];
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &[],
            "postgres",
            true,
            false,
            &schemas,
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(!script.contains("DROP SCHEMA IF EXISTS \"pg_catalog\" CASCADE"));
    }

    #[test]
    fn reset_drop_extensions() {
        let entities = sample_entities();
        let extensions = vec!["vector".to_string(), "uuid-ossp".to_string()];
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &extensions,
            "postgres",
            false,
            true,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP EXTENSION IF EXISTS \"vector\" CASCADE"));
        assert!(script.contains("DROP EXTENSION IF EXISTS \"uuid-ossp\" CASCADE"));
    }

    #[test]
    fn reset_clean_includes_schemas_and_extensions() {
        let entities = sample_entities();
        let schemas = vec!["config".to_string()];
        let extensions = vec!["vector".to_string()];
        // --clean ⇒ both drop_schemas and drop_extensions on.
        let script = build_reset_script(
            &refs(&entities),
            &[],
            &extensions,
            "postgres",
            true,
            true,
            &schemas,
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(script.contains("DROP TABLE IF EXISTS \"public\".\"users\" CASCADE"));
        assert!(script.contains("DROP SCHEMA IF EXISTS \"config\" CASCADE"));
        assert!(script.contains("DROP EXTENSION IF EXISTS \"vector\" CASCADE"));
    }

    #[test]
    fn reset_postgres_drops_roles_reverse_order() {
        let entities = sample_entities();
        let roles = vec![
            RoleEntry {
                name: "basic".to_string(),
                refers: vec![],
            },
            RoleEntry {
                name: "advanced".to_string(),
                refers: vec!["basic".to_string()],
            },
        ];
        let script = build_reset_script(
            &refs(&entities),
            &roles,
            &[],
            "postgres",
            false,
            false,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        let adv_pos = script.find("DROP ROLE IF EXISTS \"advanced\"").unwrap();
        let basic_pos = script.find("DROP ROLE IF EXISTS \"basic\"").unwrap();
        assert!(adv_pos < basic_pos);
    }

    #[test]
    fn reset_script_returns_none_when_empty() {
        let script = build_reset_script(&[], &[], &[], "postgres", false, false, &[], &HashMap::new()).unwrap();
        assert!(script.is_none());
    }

    #[test]
    fn grants_script_basic() {
        let mut schema_grants = HashMap::new();
        let mut config_grants = HashMap::new();
        config_grants.insert("anon".to_string(), vec!["usage".to_string(), "select".to_string()]);
        schema_grants.insert("config".to_string(), config_grants);

        let script = build_grants_script(&schema_grants, &[]).unwrap();
        assert!(script.contains("GRANT USAGE ON SCHEMA \"config\" TO \"anon\""));
        assert!(script.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"config\" TO \"anon\""));
        assert!(script.contains("ALTER DEFAULT PRIVILEGES"));
        assert!(script.contains("NOTIFY pgrst"));
    }

    #[test]
    fn grants_script_returns_none_when_empty() {
        let script = build_grants_script(&HashMap::new(), &[]);
        assert!(script.is_none());
    }

    /// Mirrors the merge `cmd_apply` performs: universal `schemas:` grants seed
    /// the map, then the target's `grants:` extend it — adding new roles and
    /// overriding (not unioning) the perm list for roles present in both.
    #[test]
    fn grants_script_target_overrides_universal_role_perms() {
        let mut schema_grants: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
        schema_grants.insert(
            "config".to_string(),
            HashMap::from([("app_user".to_string(), vec!["usage".to_string()])]),
        );

        let target_grants: HashMap<String, Vec<String>> = HashMap::from([
            ("app_user".to_string(), vec!["usage".to_string(), "select".to_string()]),
            (
                "app_admin".to_string(),
                vec!["usage".to_string(), "select".to_string(), "insert".to_string()],
            ),
        ]);
        schema_grants
            .entry("config".to_string())
            .or_default()
            .extend(target_grants);

        let script = build_grants_script(&schema_grants, &[]).unwrap();
        assert!(script.contains("GRANT USAGE ON SCHEMA \"config\" TO \"app_user\""));
        assert!(script.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"config\" TO \"app_user\""));
        assert!(script.contains("GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA \"config\" TO \"app_admin\""));
    }
}
