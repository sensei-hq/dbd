//! Emitting a PostgreSQL project as another engine's DDL (#23).
//!
//! # The rule these tests encode
//!
//! A construct the target cannot express is **downgraded, not refused** — the
//! output is always a complete schema — and every *lossy* downgrade is
//! reported twice: as a comment at the site in the emitted file, and in a
//! machine-readable list.
//!
//! The second half matters as much as the first. If nothing is refused, the
//! report is the only safeguard against DDL that applies cleanly and means
//! something different. So a downgrade that is emitted and not reported is the
//! bug to hunt for, and several tests below exist only to catch it.
//!
//! The converse is equally load-bearing: a **faithful** mapping must NOT be
//! reported. MySQL has a native `ENUM`, so emitting one loses nothing — listing
//! it would pad the report until nobody reads the entries that matter.
//!
//! # Only PostgreSQL can be the source
//!
//! `ParserChoice::produces_structure()` is true for libpg_query alone; the
//! statement-head readers know a table's name and not one of its columns. So
//! this is one-directional by construction, and asking to emit *from* a T-SQL
//! project is refused rather than silently emitting nothing.

use dbd_core::Design;
use dbd_core::emit_dialect::{Downgrade, emit_schema};
use dbd_core::parser::Dialect;
use std::path::Path;

/// A PostgreSQL project exercising the constructs that map differently.
fn project(dir: &Path) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: emitted\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("orders.ddl"),
        "set search_path to app;\n\
         create table if not exists orders (\n  \
           id        integer primary key\n, \
           code      varchar(20) not null unique\n, \
           total     numeric(10,2) not null default 0\n, \
           tags      text[]\n, \
           payload   jsonb not null default '{}'\n, \
           created   timestamptz not null default now()\n, \
           note      text\n\
         );\n\
         create index orders_code_idx on orders (code);\n",
    )
    .unwrap();
    let v = dir.join("ddl/view/app");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("recent.ddl"),
        "set search_path to app;\ncreate or replace view recent as select id, code from orders;\n",
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn emit(dialect: Dialect) -> (String, Vec<Downgrade>) {
    let tmp = tempfile::tempdir().unwrap();
    let design = project(tmp.path());
    emit_schema(&design, dialect, None).expect("emits")
}

// ── The schema comes out whole ──────────────────────────────────────────────

#[test]
fn every_target_emits_the_tables_and_views() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        assert!(sql.to_lowercase().contains("create table"), "{dialect:?}: {sql}");
        assert!(sql.contains("orders"), "{dialect:?} lost the table: {sql}");
        assert!(sql.contains("recent"), "{dialect:?} lost the view: {sql}");
    }
}

/// Tables before the views that read them — an emitted script that cannot be
/// run in order is not a schema.
#[test]
fn a_view_is_emitted_after_the_table_it_reads() {
    let (sql, _) = emit(Dialect::MySql);
    let table = sql.find("orders").expect("table");
    let view = sql.find("recent").expect("view");
    assert!(table < view, "the view must follow its table:\n{sql}");
}

// ── Types are translated, not copied ────────────────────────────────────────

#[test]
fn postgres_types_become_the_targets_own() {
    let (mysql, _) = emit(Dialect::MySql);
    assert!(mysql.contains("JSON"), "jsonb -> JSON: {mysql}");
    assert!(!mysql.contains("jsonb"), "and the Postgres spelling is gone: {mysql}");

    let (tsql, _) = emit(Dialect::TSql);
    assert!(tsql.contains("nvarchar"), "text/varchar -> nvarchar: {tsql}");
    assert!(!tsql.to_lowercase().contains("timestamptz"), "{tsql}");

    let (sqlite, _) = emit(Dialect::Sqlite);
    assert!(sqlite.contains("TEXT"), "{sqlite}");
}

// ── SQL Server: schemas exist before they are used, one batch per statement ─

/// The script split at its `GO` lines — what sqlcmd and SSMS send as batches.
fn batches(sql: &str) -> Vec<String> {
    sql.split("\nGO\n").map(str::to_string).collect()
}

/// A batch's statements, without the comment lines around them.
fn statements(batch: &str) -> String {
    batch
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// SQL Server keeps the schema — `[app].[orders]` — so `app` has to exist
/// before the first table names it; without `CREATE SCHEMA` the script fails
/// at its first statement. And `CREATE SCHEMA` and `CREATE VIEW` must each be
/// the only statement in their batch, so the script is cut with `GO`.
#[test]
fn sql_server_creates_each_schema_it_names_in_a_batch_of_its_own() {
    let (sql, _) = emit(Dialect::TSql);
    let schema = sql
        .find("CREATE SCHEMA [app];")
        .unwrap_or_else(|| panic!("no CREATE SCHEMA:\n{sql}"));
    let table = sql.find("CREATE TABLE [app].[orders]").expect("the table");
    assert!(schema < table, "the schema must exist before the table:\n{sql}");

    let batches = batches(&sql);
    assert!(
        batches.iter().any(|b| statements(b) == "CREATE SCHEMA [app];"),
        "CREATE SCHEMA must be alone in its batch: {batches:#?}"
    );
    let view = batches
        .iter()
        .find(|b| b.contains("CREATE VIEW"))
        .expect("the view's batch");
    assert!(
        statements(view).starts_with("CREATE VIEW") && statements(view).matches(';').count() == 1,
        "CREATE VIEW must be alone in its batch: {view}"
    );
}

/// `GO` is a SQL Server tool convention; MySQL and SQLite would read it as a
/// statement and fail. Nor do they get a `CREATE SCHEMA` — their schema is
/// folded into the name, and that fold is what gets reported.
#[test]
fn only_sql_server_gets_batches_and_schemas() {
    for dialect in [Dialect::MySql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        assert!(!sql.lines().any(|l| l.trim() == "GO"), "{dialect:?}: {sql}");
        assert!(!sql.contains("CREATE SCHEMA"), "{dialect:?}: {sql}");
    }
}

// ── Lossy downgrades are reported; faithful ones are not ────────────────────

/// An array has no equivalent in any of the three, so all three must report it.
#[test]
fn an_array_column_is_downgraded_and_reported_everywhere() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, report) = emit(dialect);
        assert!(
            report.iter().any(|d| d.column.as_deref() == Some("tags")),
            "{dialect:?}: an array column must be reported: {report:?}"
        );
        assert!(
            sql.contains("-- dbd:") || sql.contains("/* dbd:"),
            "{dialect:?}: and the file must say so at the site: {sql}"
        );
    }
}

/// MySQL has a native `ENUM`, so emitting one loses nothing and must not be
/// reported. A report padded with faithful mappings is a report nobody reads.
#[test]
fn a_faithful_mapping_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("design.yaml"),
        "project:\n  name: e\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = tmp.path().join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("plain.ddl"),
        "set search_path to app;\n\
         create table if not exists plain (id integer primary key, name varchar(20) not null);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();

    let (_, report) = emit_schema(&design, Dialect::MySql, None).expect("emits");
    let type_losses: Vec<_> = report.iter().filter(|d| d.column.is_some()).collect();
    assert!(
        type_losses.is_empty(),
        "integer and varchar map faithfully to MySQL; no column should be reported: {type_losses:?}"
    );
    // The schema fold IS a real loss and must still be reported — MySQL has no
    // schemas, so `app.plain` becomes `app_plain` and every reference to it
    // has to be updated by hand.
    assert!(
        report.iter().any(|d| d.column.is_none() && d.from.contains("schema")),
        "the schema fold must be reported: {report:?}"
    );
}

/// Every report entry names where it happened, so it can be acted on.
#[test]
fn a_report_entry_says_what_was_lost_and_where() {
    let (_, report) = emit(Dialect::TSql);
    let tags = report
        .iter()
        .find(|d| d.column.as_deref() == Some("tags"))
        .expect("the array downgrade");
    assert!(!tags.entity.is_empty(), "the entity: {tags:?}");
    assert!(!tags.from.is_empty() && !tags.to.is_empty(), "from and to: {tags:?}");
    assert!(
        tags.reason.len() > 20,
        "and a reason worth reading, not a label: {tags:?}"
    );
}

/// The comment is in the target's own syntax, or the file will not parse.
#[test]
fn the_downgrade_comment_is_valid_in_the_target() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        for line in sql.lines().filter(|l| l.contains("dbd:")) {
            assert!(
                line.trim_start().starts_with("--") || line.trim_start().starts_with("/*"),
                "{dialect:?}: a note must be a comment: {line}"
            );
        }
    }
}

// ── Only PostgreSQL can be the source ───────────────────────────────────────

/// A project dbd reads without structure has no columns to emit. Refusing is
/// the honest answer; emitting an empty schema would look like success.
#[test]
fn emitting_from_a_structureless_project_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("design.yaml"),
        "project:\n  name: t\n\nsource:\n  dialect: tsql\n\nschemas:\n  - dbo\n",
    )
    .unwrap();
    let t = tmp.path().join("ddl/table/dbo");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("i.ddl"), "CREATE TABLE dbo.Issues (Id int NOT NULL);").unwrap();
    let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();

    let err = match emit_schema(&design, Dialect::MySql, None) {
        Ok((sql, _)) => panic!("must refuse, emitted: {sql}"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("structure") || err.contains("columns"), "{err}");
}

/// Emitting PostgreSQL from a PostgreSQL project is a no-op worth refusing —
/// `combine` already does that, and doing both invites confusion about which
/// produces the applyable script.
#[test]
fn emitting_the_source_dialect_points_at_combine() {
    let tmp = tempfile::tempdir().unwrap();
    let design = project(tmp.path());
    let err = match emit_schema(&design, Dialect::PostgreSql, None) {
        Ok(_) => panic!("must refuse"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("combine"),
        "it should name the command that does this: {err}"
    );
}

// ── Keys are not silently dropped ───────────────────────────────────────────

/// A column-level `PRIMARY KEY` must survive.
///
/// It did not on the first cut: a *parsed* table carries `ColumnDef::is_pk`,
/// and only the reconcile path lifts that into a `TableConstraint`. Emitting
/// constraints alone produced a table with no key at all — valid DDL, wrong
/// schema, and exactly the silent loss the report cannot catch because nothing
/// knew anything had been lost.
#[test]
fn a_column_level_primary_key_survives() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        assert!(
            sql.to_uppercase().contains("PRIMARY KEY"),
            "{dialect:?}: the primary key vanished:\n{sql}"
        );
    }
}

/// And a column-level `UNIQUE` likewise — `code varchar(20) not null unique`.
#[test]
fn a_column_level_unique_survives() {
    let (sql, _) = emit(Dialect::MySql);
    assert!(sql.to_uppercase().contains("UNIQUE"), "the unique vanished:\n{sql}");
}

/// A key is a faithful mapping in all three targets, so it must not be
/// reported — only things that actually changed meaning belong in the report.
#[test]
fn a_surviving_key_is_not_reported_as_a_downgrade() {
    let (_, report) = emit(Dialect::MySql);
    assert!(
        !report.iter().any(|d| d.to.to_uppercase().contains("PRIMARY KEY")),
        "a key that survived is not a downgrade: {report:?}"
    );
}

// ── Keys, checks and indexes come across ────────────────────────────────────

/// A project with the constraints and indexes `emit` used to drop without a
/// word: an inline FK with an action, a table-level FK, column and table
/// CHECKs, and indexes plain, expression and partial.
fn keyed_project(dir: &Path) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: keyed\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("customers.ddl"),
        "set search_path to app;\n\
         create table if not exists customers (\n  id integer primary key\n, code varchar(20) not null\n, email text not null\n);\n\
         create unique index customers_code_key on customers (code);\n\
         create unique index customers_email_key on customers (lower(email));\n",
    )
    .unwrap();
    std::fs::write(
        t.join("orders.ddl"),
        "set search_path to app;\n\
         create table if not exists orders (\n  \
           id          integer primary key\n, \
           customer_id integer not null references customers (id) on delete cascade\n, \
           parent_id   integer\n, \
           total       numeric(10,2) not null check (total >= 0)\n, \
           status      text not null\n, \
           constraint orders_parent_fk foreign key (parent_id) references orders (id)\n, \
           constraint orders_status_check check (status in ('open', 'paid'))\n\
         );\n\
         create index orders_customer_idx on orders (customer_id);\n\
         create index orders_open_idx on orders (status) where status = 'open';\n",
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn emit_keyed(dialect: Dialect) -> (String, Vec<Downgrade>) {
    let tmp = tempfile::tempdir().unwrap();
    let design = keyed_project(tmp.path());
    emit_schema(&design, dialect, None).expect("emits")
}

/// Every target has FOREIGN KEY, so a key is carried, never dropped — an
/// emitted schema without its keys applies cleanly and enforces nothing.
#[test]
fn foreign_keys_are_emitted_on_every_target() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit_keyed(dialect);
        let upper = sql.to_uppercase();
        assert_eq!(
            upper.matches("FOREIGN KEY").count(),
            2,
            "{dialect:?}: the inline and the table-level key: {sql}"
        );
        assert!(
            upper.contains("ON DELETE CASCADE"),
            "{dialect:?}: the action comes too: {sql}"
        );
    }
}

/// CHECK exists everywhere, but its expression is PostgreSQL SQL that dbd does
/// not translate — so it is emitted AND reported, the way a view body is.
#[test]
fn checks_are_emitted_and_their_untranslated_expression_reported() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, report) = emit_keyed(dialect);
        assert_eq!(
            sql.to_uppercase().matches("CHECK (").count(),
            2,
            "{dialect:?}: the column and the table check: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity.ends_with("orders") && d.from.contains("CHECK")),
            "{dialect:?}: an untranslated CHECK must be reported: {report:?}"
        );
    }
}

/// Indexes are part of the schema: a plain one is emitted as-is, and what a
/// target cannot express — an expression key, a partial predicate — is
/// reported rather than lost. One SQL Server cannot build at all (an
/// expression key) is left out, and the report says so.
#[test]
fn indexes_are_emitted_and_what_cannot_carry_is_reported() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, report) = emit_keyed(dialect);
        let upper = sql.to_uppercase();
        assert!(upper.contains("CREATE INDEX"), "{dialect:?}: plain index: {sql}");
        assert!(sql.contains("orders_customer_idx"), "{dialect:?}: {sql}");
        assert!(
            upper.contains("CREATE UNIQUE INDEX"),
            "{dialect:?}: unique index: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity.ends_with("customers") && d.from.contains("expression")),
            "{dialect:?}: an expression key must be reported: {report:?}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity.ends_with("orders") && d.from.contains("WHERE")),
            "{dialect:?}: a partial index predicate must be reported: {report:?}"
        );
    }
    let (tsql, report) = emit_keyed(Dialect::TSql);
    assert!(!tsql.contains("CREATE UNIQUE INDEX [customers_email_key]"), "{tsql}");
    assert!(
        report
            .iter()
            .any(|d| d.from.contains("customers_email_key") && d.to == "no index"),
        "SQL Server leaves the expression index out and says so: {report:?}"
    );
}

// ── Building blocks for the tests below ─────────────────────────────────────

const ALL: [Dialect; 3] = [Dialect::MySql, Dialect::TSql, Dialect::Sqlite];

/// A project of `files` — each `(path under ddl/, sql)`, read under
/// `search_path app` — with `extra` appended to `design.yaml` (an `external:`
/// or `scopes:` block).
fn project_with(dir: &Path, extra: &str, files: &[(&str, &str)]) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        format!(
            "project:\n  name: p\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n{extra}"
        ),
    )
    .unwrap();
    for (path, sql) in files {
        let file = dir.join("ddl").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, format!("set search_path to app;\n{sql}\n")).unwrap();
    }
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

/// [`project_with`], emitted — under `scope` when one is named.
fn emit_with(dialect: Dialect, extra: &str, files: &[(&str, &str)], scope: Option<&str>) -> (String, Vec<Downgrade>) {
    let tmp = tempfile::tempdir().unwrap();
    let design = project_with(tmp.path(), extra, files);
    let resolved = scope.map(|s| design.resolve_scope(Some(s), None).expect("scope"));
    emit_schema(&design, dialect, resolved.as_ref()).expect("emits")
}

// ── A key to a table the script does not create is left out ────────────────

/// `auth.users` is someone else's table: declared `external:`, never emitted.
/// A key to it is a statement the target refuses (SQL Server and MySQL check
/// the parent exists), so the script would not apply on its own.
#[test]
fn a_foreign_key_to_an_external_table_is_left_out_and_reported() {
    let files = [(
        "table/app/profiles.ddl",
        "create table profiles (id integer primary key, user_id uuid not null references auth.users (id));",
    )];
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "\nexternal:\n  - name: auth.users\n", &files, None);
        assert!(
            !statements(&sql).to_uppercase().contains("FOREIGN KEY"),
            "{dialect:?}: a key to an external table cannot apply: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity == "app.profiles" && d.from.contains("auth.users") && d.to == "no foreign key"),
            "{dialect:?}: and leaving it out must be reported: {report:?}"
        );
    }
}

/// Under `--scope` the parent may simply not be in the script. The key comes
/// out with it, and says so — the emitted script must apply on its own.
#[test]
fn a_foreign_key_to_a_table_outside_the_scope_is_left_out_and_reported() {
    let files = [
        (
            "table/app/customers.ddl",
            "create table customers (id integer primary key);",
        ),
        (
            "table/app/orders.ddl",
            "create table orders (id integer primary key, customer_id integer references customers (id));",
        ),
    ];
    let scopes = "\nscopes:\n  just_orders:\n    includes: [app.orders]\n";
    for dialect in ALL {
        let (whole, _) = emit_with(dialect, scopes, &files, None);
        assert!(
            statements(&whole).to_uppercase().contains("FOREIGN KEY"),
            "{dialect:?}: with its parent in the script, the key stays: {whole}"
        );

        let (sql, report) = emit_with(dialect, scopes, &files, Some("just_orders"));
        assert!(
            !sql.lines()
                .any(|l| l.starts_with("CREATE TABLE") && l.contains("customers")),
            "{dialect:?}: the scope leaves the parent out: {sql}"
        );
        assert!(
            !statements(&sql).to_uppercase().contains("FOREIGN KEY"),
            "{dialect:?}: the parent is not in the script: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity == "app.orders" && d.from.contains("app.customers") && d.to == "no foreign key"),
            "{dialect:?}: {report:?}"
        );
    }
}

// ── The emitted SQLite script applies ───────────────────────────────────────

/// Apply `script` to a fresh in-memory SQLite database with foreign keys
/// enforced, run `then`, and return the one text value `query` selects.
///
/// The end-to-end check the other targets cannot have here: the script either
/// lands on a real engine or the error says which statement did not.
#[cfg(feature = "sqlite")]
fn on_sqlite(script: &str, then: &str, query: &str) -> Result<String, String> {
    use sqlx::Connection;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .map_err(|e| e.to_string())?;
        sqlx::raw_sql("PRAGMA foreign_keys = ON;")
            .execute(&mut db)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::raw_sql(script)
            .execute(&mut db)
            .await
            .map_err(|e| format!("the script does not apply: {e}\n{script}"))?;
        if !then.is_empty() {
            sqlx::raw_sql(then)
                .execute(&mut db)
                .await
                .map_err(|e| format!("`{then}` failed: {e}\n{script}"))?;
        }
        sqlx::query_scalar::<_, String>(query)
            .fetch_one(&mut db)
            .await
            .map_err(|e| format!("`{query}` failed: {e}"))
    })
}

/// Every fixture in this file, emitted for SQLite, is a script SQLite accepts.
#[cfg(feature = "sqlite")]
#[test]
fn the_emitted_sqlite_script_applies() {
    let external = [(
        "table/app/profiles.ddl",
        "create table profiles (id integer primary key, user_id uuid not null references auth.users (id));",
    )];
    for (label, sql) in [
        ("orders", emit(Dialect::Sqlite).0),
        ("keyed", emit_keyed(Dialect::Sqlite).0),
        (
            "external",
            emit_with(Dialect::Sqlite, "\nexternal:\n  - name: auth.users\n", &external, None).0,
        ),
        ("defaults", emit_with(Dialect::Sqlite, "", &DEFAULTS, None).0),
    ] {
        if let Err(e) = on_sqlite(&sql, "", "select 'ok'") {
            panic!("{label}: {e}");
        }
    }
}

// ── A default is translated, or reported ───────────────────────────────────

/// Defaults of every shape: literals behind a cast, the clock, a random UUID,
/// and a function call nothing translates.
const DEFAULTS: [(&str, &str); 1] = [(
    "table/app/things.ddl",
    "create table things (\n  \
       id      integer primary key\n, \
       uid     uuid not null default gen_random_uuid()\n, \
       status  text not null default 'open'::text\n, \
       path    varchar(40) default 'a\\b'\n, \
       made    date default current_date\n, \
       at      timestamptz default now()\n, \
       flag    boolean not null default true\n, \
       digest  text default md5(random()::text)\n\
     );",
)];

/// `md5(random()::text)` is PostgreSQL SQL, and dbd does not translate
/// expressions. Dropping it was right — the target may not have the function —
/// but dropping it without a word left a column that is silently NULL.
#[test]
fn a_default_nothing_translates_is_dropped_and_reported() {
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &DEFAULTS, None);
        assert!(
            !statements(&sql).contains("md5"),
            "{dialect:?}: it is not emitted: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.column.as_deref() == Some("digest") && d.from.contains("md5") && d.to == "no default"),
            "{dialect:?}: and dropping it is reported: {report:?}"
        );
    }
}

/// `'open'::text` is a literal behind a PostgreSQL cast. `::` is a syntax error
/// on all three targets; the literal is what the default means.
#[test]
fn a_cast_literal_default_keeps_the_literal_and_drops_the_cast() {
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &DEFAULTS, None);
        assert!(!statements(&sql).contains("::"), "{dialect:?}: {sql}");
        assert!(sql.contains("'open'"), "{dialect:?}: {sql}");
        assert!(
            !report.iter().any(|d| d.column.as_deref() == Some("status")),
            "{dialect:?}: a literal is faithful, not a downgrade: {report:?}"
        );
    }
}

/// The defaults with a faithful equivalent are translated, not dropped.
#[test]
fn the_clock_and_a_random_uuid_are_translated() {
    let (tsql, report) = emit_with(Dialect::TSql, "", &DEFAULTS, None);
    assert!(tsql.contains("DEFAULT NEWID()"), "gen_random_uuid() is NEWID(): {tsql}");
    assert!(
        tsql.contains("[made] date DEFAULT CONVERT(date, SYSDATETIME())"),
        "{tsql}"
    );
    assert!(
        !report
            .iter()
            .any(|d| d.column.as_deref() == Some("uid") || d.column.as_deref() == Some("made")),
        "both are faithful on SQL Server: {report:?}"
    );

    let (mysql, report) = emit_with(Dialect::MySql, "", &DEFAULTS, None);
    assert!(mysql.contains("DEFAULT (CURRENT_DATE)"), "{mysql}");
    // MySQL's UUID() is version 1 — time and host, not random — so it is the
    // nearest thing, and reported.
    assert!(mysql.contains("DEFAULT (UUID())"), "{mysql}");
    assert!(
        report
            .iter()
            .any(|d| d.column.as_deref() == Some("uid") && d.from.contains("gen_random_uuid")),
        "{report:?}"
    );
}

/// MySQL takes a default on a TEXT column only as an expression — `('open')`,
/// not `'open'` — and reads a backslash in a string as an escape, which a
/// PostgreSQL literal does not. `text` is a LONGTEXT there (see
/// `text_and_bytea_get_a_mysql_type_as_large_as_postgres_allows`), and the
/// rule covers every TEXT size.
#[test]
fn mysql_gets_a_text_default_as_an_expression_and_its_backslashes_escaped() {
    let (mysql, _) = emit_with(Dialect::MySql, "", &DEFAULTS, None);
    assert!(mysql.contains("`status` LONGTEXT NOT NULL DEFAULT ('open')"), "{mysql}");
    assert!(mysql.contains(r"DEFAULT 'a\\b'"), "{mysql}");
}

/// On SQLite the translated defaults are checked by the engine itself: a row
/// given only its key gets a UUID-shaped id, the literal, today, and `1`.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_fills_the_translated_defaults() {
    let (sql, _) = emit_with(Dialect::Sqlite, "", &DEFAULTS, None);
    let row = on_sqlite(
        &sql,
        "insert into app_things (id) values (1);",
        "select length(uid) || '|' || substr(uid, 15, 1) || '|' || status || '|' || path || '|' \
         || (made = date('now')) || '|' || flag from app_things",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(row, r"36|4|open|a\b|1|1", "{sql}");
}

// ── Identity columns keep generating their values ──────────────────────────

const ALWAYS: [(&str, &str); 1] = [(
    "table/app/events.ddl",
    "create table events (id bigint generated always as identity primary key, name text);",
)];
const BY_DEFAULT: [(&str, &str); 1] = [(
    "table/app/tags.ddl",
    "create table tags (id integer generated by default as identity primary key, name text);",
)];
// Declared the key: `serial` alone is not one (Postgres makes it NOT NULL with a
// default, nothing more), and MySQL numbers only a key column.
const SERIAL: [(&str, &str); 1] = [(
    "table/app/notes.ddl",
    "create table notes (id serial primary key, body text);",
)];

/// The report entries about column `column`.
fn about<'a>(report: &'a [Downgrade], column: &str) -> Vec<&'a Downgrade> {
    report.iter().filter(|d| d.column.as_deref() == Some(column)).collect()
}

/// An identity column was emitted as a plain integer: valid DDL, and every
/// insert that relied on the database to number the row now fails NOT NULL.
/// Each target has its own way to generate the value.
#[test]
fn an_identity_column_is_generated_on_every_target() {
    let (mysql, _) = emit_with(Dialect::MySql, "", &BY_DEFAULT, None);
    assert!(mysql.contains("`id` INT NOT NULL AUTO_INCREMENT"), "{mysql}");

    let (tsql, _) = emit_with(Dialect::TSql, "", &ALWAYS, None);
    assert!(tsql.contains("[id] bigint IDENTITY(1,1) NOT NULL"), "{tsql}");

    // SQLite generates a value only for a column declared INTEGER PRIMARY KEY
    // in place — AUTOINCREMENT is refused on a table-level key — so the key
    // moves onto the column and is not declared twice.
    let (sqlite, _) = emit_with(Dialect::Sqlite, "", &BY_DEFAULT, None);
    assert!(sqlite.contains(r#""id" INTEGER PRIMARY KEY AUTOINCREMENT"#), "{sqlite}");
    assert!(!sqlite.contains(r#"PRIMARY KEY ("id")"#), "{sqlite}");
}

/// ALWAYS refuses an explicit value and BY DEFAULT takes one. MySQL's and
/// SQLite's counters take one, so ALWAYS loses its guard there; SQL Server's
/// IDENTITY refuses one, so BY DEFAULT is what loses there. The mapping that
/// keeps the meaning is not reported.
#[test]
fn always_and_by_default_are_reported_where_the_target_differs() {
    for (dialect, files, reported) in [
        (Dialect::MySql, ALWAYS, true),
        (Dialect::MySql, BY_DEFAULT, false),
        (Dialect::TSql, ALWAYS, false),
        (Dialect::TSql, BY_DEFAULT, true),
        (Dialect::Sqlite, ALWAYS, true),
        (Dialect::Sqlite, BY_DEFAULT, false),
    ] {
        let (sql, report) = emit_with(dialect, "", &files, None);
        let entries = about(&report, "id");
        assert_eq!(
            !entries.is_empty(),
            reported,
            "{dialect:?} {}: {entries:?}\n{sql}",
            files[0].1
        );
        if reported {
            assert!(
                entries.iter().any(|d| d.from.contains("IDENTITY")),
                "{dialect:?}: names the identity: {entries:?}"
            );
        }
    }
}

/// `serial` is an integer drawing from its own sequence — an identity by
/// another name, taking explicit values like BY DEFAULT.
#[test]
fn a_serial_column_is_generated_like_a_by_default_identity() {
    let (mysql, report) = emit_with(Dialect::MySql, "", &SERIAL, None);
    assert!(mysql.contains("`id` INT NOT NULL AUTO_INCREMENT"), "{mysql}");
    assert!(about(&report, "id").is_empty(), "{report:?}");

    let (tsql, report) = emit_with(Dialect::TSql, "", &SERIAL, None);
    assert!(tsql.contains("[id] int IDENTITY(1,1) NOT NULL"), "{tsql}");
    assert!(
        about(&report, "id").iter().any(|d| d.from.contains("serial")),
        "{report:?}"
    );

    let (sqlite, _) = emit_with(Dialect::Sqlite, "", &SERIAL, None);
    assert!(sqlite.contains(r#""id" INTEGER PRIMARY KEY AUTOINCREMENT"#), "{sqlite}");
}

/// MySQL generates values only for a column that leads a key, and SQLite only
/// for a single-column INTEGER PRIMARY KEY. An identity on any other column is
/// a plain column there — and reported, since inserts must now supply it.
#[test]
fn an_identity_the_target_cannot_generate_is_reported() {
    let files = [(
        "table/app/logs.ddl",
        "create table logs (id integer primary key, n bigint generated by default as identity);",
    )];
    for dialect in [Dialect::MySql, Dialect::Sqlite] {
        let (sql, report) = emit_with(dialect, "", &files, None);
        assert!(!sql.contains("AUTO"), "{dialect:?}: {sql}");
        assert!(
            about(&report, "n").iter().any(|d| d.to == "a plain column"),
            "{dialect:?}: {report:?}"
        );
    }
    let (tsql, _) = emit_with(Dialect::TSql, "", &files, None);
    assert!(tsql.contains("[n] bigint IDENTITY(1,1) NOT NULL"), "{tsql}");
}

/// SQLite itself numbers the rows, and like a sequence never hands an id out
/// twice. A bare INTEGER PRIMARY KEY numbers them too but reuses the top id
/// once its row is deleted — which is why it takes AUTOINCREMENT.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_numbers_identity_and_serial_rows_without_reusing_an_id() {
    for (files, table) in [(ALWAYS, "app_events"), (BY_DEFAULT, "app_tags"), (SERIAL, "app_notes")] {
        let (sql, _) = emit_with(Dialect::Sqlite, "", &files, None);
        let col = if table == "app_notes" { "body" } else { "name" };
        let ids = on_sqlite(
            &sql,
            &format!(
                "insert into {table} ({col}) values ('a'); insert into {table} ({col}) values ('b'); \
                 delete from {table} where id = 2; insert into {table} ({col}) values ('c');"
            ),
            &format!("select group_concat(id, ',') from {table}"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ids, "1,3", "{sql}");
    }
}

// ── Sequences, and the defaults that draw from them ────────────────────────

const SEQUENCED: [(&str, &str); 2] = [
    (
        "sequence/app/counter.ddl",
        "create sequence counter start with 1000 increment by 5;",
    ),
    (
        "table/app/tickets.ddl",
        "create table tickets (\n  \
           id   bigint primary key default nextval('counter')\n, \
           ref  bigint default nextval('app.counter')\n, \
           note text\n\
         );",
    ),
];

/// SQL Server has CREATE SEQUENCE, so the sequence is emitted — with every
/// bound spelled out, because SQL Server's defaults are not PostgreSQL's: its
/// MINVALUE, and so its first value, is the type's minimum, -2^63 for bigint.
/// A column that drew from it draws from it still.
#[test]
fn sql_server_gets_the_sequence_and_the_columns_that_draw_from_it() {
    let (sql, report) = emit_with(Dialect::TSql, "", &SEQUENCED, None);
    let seq = sql
        .find("CREATE SEQUENCE [app].[counter] AS bigint")
        .unwrap_or_else(|| panic!("no CREATE SEQUENCE:\n{sql}"));
    let table = sql.find("CREATE TABLE [app].[tickets]").expect("the table");
    assert!(seq < table, "the sequence must exist before its users:\n{sql}");
    for part in [
        "START WITH 1000",
        "INCREMENT BY 5",
        "MINVALUE 1 ",
        "NO CYCLE",
        "NO CACHE",
    ] {
        assert!(sql.contains(part), "missing `{part}`:\n{sql}");
    }
    assert!(
        sql.contains("[id] bigint NOT NULL DEFAULT NEXT VALUE FOR [app].[counter]"),
        "{sql}"
    );
    assert!(
        sql.contains("[ref] bigint DEFAULT NEXT VALUE FOR [app].[counter]"),
        "{sql}"
    );
    assert!(
        report
            .iter()
            .all(|d| d.entity != "app.counter" && d.entity != "app.tickets"),
        "all faithful: {report:?}"
    );
}

/// MySQL and SQLite have no sequences. The sequence is reported; a key column
/// that drew from it gets the table's own counter — the nearest thing, and
/// reported — and any other column loses the default, reported.
#[test]
fn without_sequences_the_sequence_and_its_defaults_are_reported() {
    for (dialect, counter) in [(Dialect::MySql, "AUTO_INCREMENT"), (Dialect::Sqlite, "AUTOINCREMENT")] {
        let (sql, report) = emit_with(dialect, "", &SEQUENCED, None);
        assert!(!statements(&sql).contains("SEQUENCE"), "{dialect:?}: {sql}");
        assert!(!statements(&sql).contains("nextval"), "{dialect:?}: {sql}");
        assert!(
            report.iter().any(|d| d.entity == "app.counter" && d.to == "nothing"),
            "{dialect:?}: the sequence: {report:?}"
        );
        assert!(sql.contains(counter), "{dialect:?}: the key is numbered: {sql}");
        assert!(
            about(&report, "id").iter().any(|d| d.from.contains("nextval")),
            "{dialect:?}: {report:?}"
        );
        assert!(
            about(&report, "ref")
                .iter()
                .any(|d| d.from.contains("nextval") && d.to == "no default"),
            "{dialect:?}: {report:?}"
        );
    }
}

/// Under `--scope` the sequence may not be in the script; a default that names
/// it cannot apply on its own.
#[test]
fn a_default_drawing_from_a_sequence_not_in_the_script_is_dropped_and_reported() {
    let scopes = "\nscopes:\n  tickets:\n    includes: [app.tickets]\n";
    let (sql, report) = emit_with(Dialect::TSql, scopes, &SEQUENCED, Some("tickets"));
    assert!(!sql.contains("NEXT VALUE FOR"), "{sql}");
    assert!(
        about(&report, "ref")
            .iter()
            .any(|d| d.to == "no default" && d.reason.contains("app.counter")),
        "{report:?}"
    );
}

/// On SQLite the table applies and numbers its rows the way a sequence would —
/// never handing an id out twice, even once the top row is deleted.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_numbers_a_column_that_drew_from_a_sequence() {
    let (sql, _) = emit_with(Dialect::Sqlite, "", &SEQUENCED, None);
    let ids = on_sqlite(
        &sql,
        "insert into app_tickets (note) values ('a'); insert into app_tickets (note) values ('b'); \
         delete from app_tickets where id = 2; insert into app_tickets (note) values ('c');",
        "select group_concat(id, ',') from app_tickets",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids, "1,3", "{sql}");
}

/// PostgreSQL's defaults depend on direction: a descending sequence starts at
/// -1 and runs down to the type's minimum. Every bound is resolved and spelled
/// out, the type and CACHE/CYCLE carry, and an `OWNED BY` — which ties the
/// sequence's life to a column, and has no SQL Server equivalent — is reported.
#[test]
fn sql_server_gets_a_sequence_with_postgres_bounds_spelled_out() {
    let files = [
        (
            "sequence/app/countdown.ddl",
            "create sequence countdown as integer increment by -1 cache 20 cycle;",
        ),
        (
            "sequence/app/owned.ddl",
            "create sequence owned as smallint owned by app.things.id;",
        ),
    ];
    let (sql, report) = emit_with(Dialect::TSql, "", &files, None);
    assert!(
        sql.contains(
            "CREATE SEQUENCE [app].[countdown] AS int START WITH -1 INCREMENT BY -1 \
             MINVALUE -2147483648 MAXVALUE -1 CYCLE CACHE 20;"
        ),
        "{sql}"
    );
    assert!(
        sql.contains(
            "CREATE SEQUENCE [app].[owned] AS smallint START WITH 1 INCREMENT BY 1 \
             MINVALUE 1 MAXVALUE 32767 NO CYCLE NO CACHE;"
        ),
        "{sql}"
    );
    assert!(
        report
            .iter()
            .any(|d| d.entity == "app.owned" && d.from.contains("OWNED BY")),
        "{report:?}"
    );
    assert!(!report.iter().any(|d| d.entity == "app.countdown"), "{report:?}");
}

// ── Generated columns stay generated ───────────────────────────────────────

const GENERATED: [(&str, &str); 1] = [(
    "table/app/lines.ddl",
    "create table lines (\n  \
       id    integer primary key\n, \
       price numeric(10,2) not null\n, \
       qty   integer not null\n, \
       total numeric(12,2) generated always as (price * qty) stored\n\
     );",
)];

/// A generated column was emitted as a plain one: it applied, and then held
/// whatever an insert put there instead of the value it is defined as. All
/// three targets compute stored columns, so it is carried — with its
/// expression untranslated, and reported, like a CHECK.
#[test]
fn a_generated_column_is_computed_on_every_target_and_its_expression_reported() {
    let (mysql, _) = emit_with(Dialect::MySql, "", &GENERATED, None);
    assert!(
        mysql.contains("`total` DECIMAL(12,2) GENERATED ALWAYS AS (price * qty) STORED"),
        "{mysql}"
    );
    // SQL Server's computed column takes its type from the expression, so the
    // declared one is kept with a CAST.
    let (tsql, _) = emit_with(Dialect::TSql, "", &GENERATED, None);
    assert!(
        tsql.contains("[total] AS CAST((price * qty) AS decimal(12,2)) PERSISTED"),
        "{tsql}"
    );
    let (sqlite, _) = emit_with(Dialect::Sqlite, "", &GENERATED, None);
    assert!(
        sqlite.contains(r#""total" NUMERIC GENERATED ALWAYS AS (price * qty) STORED"#),
        "{sqlite}"
    );

    for dialect in ALL {
        let (_, report) = emit_with(dialect, "", &GENERATED, None);
        assert!(
            about(&report, "total")
                .iter()
                .any(|d| d.from.contains("price * qty") && d.to == "the same text, untranslated"),
            "{dialect:?}: {report:?}"
        );
    }
}

/// SQLite computes it: a row given a price and a quantity has their product.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_computes_a_generated_column() {
    let (sql, _) = emit_with(Dialect::Sqlite, "", &GENERATED, None);
    let total = on_sqlite(
        &sql,
        "insert into app_lines (id, price, qty) values (1, 2.5, 4);",
        "select cast(total as text) from app_lines",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(total, "10", "{sql}");
}

// ── A materialized view becomes a view, and says so ────────────────────────

const MATVIEW: [(&str, &str); 2] = [
    (
        "table/app/orders.ddl",
        "create table orders (id integer primary key, total numeric(10,2));",
    ),
    (
        "materialized_view/app/order_totals.ddl",
        "create materialized view order_totals as select id, total from orders with data;\n\
         create unique index order_totals_id_idx on order_totals (id);",
    ),
];

/// None of the three has materialized views, so one becomes a plain view —
/// the query runs on every read instead of serving stored rows. That is a
/// loss, reported. Its indexes (a unique one is what REFRESH CONCURRENTLY
/// needs) cannot apply to a plain view: each is left out, and reported.
#[test]
fn a_materialized_view_becomes_a_view_and_its_indexes_are_reported() {
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &MATVIEW, None);
        let stmts = statements(&sql);
        assert!(stmts.contains("CREATE VIEW"), "{dialect:?}: {sql}");
        assert!(!stmts.contains("INDEX"), "{dialect:?}: a view cannot be indexed: {sql}");
        assert!(
            report.iter().any(|d| d.entity == "app.order_totals"
                && d.from.contains("materialized view")
                && d.to == "a plain view"),
            "{dialect:?}: {report:?}"
        );
        assert!(
            report.iter().any(|d| d.entity == "app.order_totals"
                && d.from.contains("order_totals_id_idx")
                && d.to == "no index"),
            "{dialect:?}: {report:?}"
        );
        assert!(
            sql.lines()
                .any(|l| l.starts_with("-- dbd:") && l.contains("order_totals_id_idx")),
            "{dialect:?}: and the file says so: {sql}"
        );
    }
}

// ── Comments are carried where the target keeps them ───────────────────────

const COMMENTED: [(&str, &str); 2] = [
    (
        "table/app/people.ddl",
        "create table people (id integer primary key, name text);\n\
         comment on table people is 'Everyone we know';\n\
         comment on column people.name is 'What they''re called\nin full';",
    ),
    (
        "view/app/adults.ddl",
        "create view adults as select id from people;\n\
         comment on view adults is 'People over 18';",
    ),
];

/// MySQL keeps a comment on a table and on a column, so both come across —
/// escaped for MySQL, which reads a backslash in a string as an escape.
#[test]
fn mysql_carries_table_and_column_comments() {
    let (sql, report) = emit_with(Dialect::MySql, "", &COMMENTED, None);
    assert!(
        sql.contains(r"`name` LONGTEXT COMMENT 'What they''re called\nin full'"),
        "{sql}"
    );
    assert!(sql.contains(") COMMENT='Everyone we know';"), "{sql}");
    assert!(
        !report
            .iter()
            .any(|d| d.entity == "app.people" && d.from.contains("comment")),
        "carried faithfully: {report:?}"
    );
}

/// SQL Server keeps comments as `MS_Description` extended properties — what
/// SSMS shows — on the table, the column and the view.
#[test]
fn sql_server_carries_comments_as_ms_description_properties() {
    let (sql, report) = emit_with(Dialect::TSql, "", &COMMENTED, None);
    let property = |value: &str, level1: &str, name: &str, column: Option<&str>| {
        let mut s = format!(
            "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'{value}', \
             @level0type = N'SCHEMA', @level0name = N'app', @level1type = N'{level1}', @level1name = N'{name}'"
        );
        if let Some(c) = column {
            s.push_str(&format!(", @level2type = N'COLUMN', @level2name = N'{c}'"));
        }
        s + ";"
    };
    for expected in [
        property("Everyone we know", "TABLE", "people", None),
        property("What they''re called\nin full", "TABLE", "people", Some("name")),
        property("People over 18", "VIEW", "adults", None),
    ] {
        assert!(sql.contains(&expected), "missing {expected}\n{sql}");
    }
    assert!(!report.iter().any(|d| d.from.contains("comment")), "{report:?}");
}

/// SQLite keeps no comments, and MySQL none on a view. Each one dropped is
/// reported — it was documentation someone wrote, and it is gone.
#[test]
fn a_comment_the_target_cannot_keep_is_reported() {
    let (_, sqlite) = emit_with(Dialect::Sqlite, "", &COMMENTED, None);
    for (entity, column) in [("app.people", None), ("app.people", Some("name")), ("app.adults", None)] {
        assert!(
            sqlite.iter().any(|d| d.entity == entity
                && d.column.as_deref() == column
                && d.from.contains("comment")
                && d.to == "nothing"),
            "SQLite, {entity} {column:?}: {sqlite:?}"
        );
    }
    let (_, mysql) = emit_with(Dialect::MySql, "", &COMMENTED, None);
    assert!(
        mysql
            .iter()
            .any(|d| d.entity == "app.adults" && d.from.contains("comment") && d.to == "nothing"),
        "MySQL has no view comments: {mysql:?}"
    );
}

/// A note quotes what was lost, and a comment can span lines. Every line of
/// a note must still be a comment, or the script breaks at the second one.
#[cfg(feature = "sqlite")]
#[test]
fn a_note_quoting_a_multi_line_comment_stays_a_comment() {
    let (sql, _) = emit_with(Dialect::Sqlite, "", &COMMENTED, None);
    on_sqlite(&sql, "", "select 'ok'").unwrap_or_else(|e| panic!("{e}"));
}

// ── A key on unbounded text is bounded, not dropped ────────────────────────

const TEXT_KEYS: [(&str, &str); 2] = [
    (
        "table/app/countries.ddl",
        "create table countries (code text primary key, name text not null unique);",
    ),
    (
        "table/app/cities.ddl",
        "create table cities (\n  \
           id      integer primary key\n, \
           country text not null references countries (code)\n, \
           note    text\n\
         );",
    ),
];

/// MySQL refuses a TEXT column in a PRIMARY KEY, UNIQUE or FOREIGN KEY without
/// a prefix length, and SQL Server refuses nvarchar(max) in any key. Leaving
/// the constraint out would lose the key and every foreign key that points at
/// it, so the column is bounded instead — VARCHAR(255) / nvarchar(450), the
/// most a 900-byte SQL Server key holds — and the bound is reported: a longer
/// value is now refused. A foreign key's own column gets the same bound, so
/// the two sides still match.
#[test]
fn a_key_on_unbounded_text_is_bounded_and_reported() {
    for (dialect, bounded) in [(Dialect::MySql, "VARCHAR(255)"), (Dialect::TSql, "nvarchar(450)")] {
        let (sql, report) = emit_with(dialect, "", &TEXT_KEYS, None);
        let stmts = statements(&sql);
        for key in ["PRIMARY KEY", "UNIQUE", "FOREIGN KEY"] {
            assert!(stmts.contains(key), "{dialect:?}: the {key} stays: {sql}");
        }
        for (entity, column) in [
            ("app.countries", "code"),
            ("app.countries", "name"),
            ("app.cities", "country"),
        ] {
            assert!(
                report
                    .iter()
                    .any(|d| d.entity == entity && d.column.as_deref() == Some(column) && d.to == bounded),
                "{dialect:?}: {entity}.{column} is bounded to {bounded} and reported: {report:?}"
            );
        }
        assert!(
            about(&report, "note").is_empty(),
            "{dialect:?}: a column in no key keeps its type: {report:?}"
        );
    }
    let (sqlite, report) = emit_with(Dialect::Sqlite, "", &TEXT_KEYS, None);
    assert!(
        report.iter().all(|d| d.column.is_none()),
        "SQLite keys TEXT as it is: {report:?}\n{sqlite}"
    );
}

/// Once a key column is bounded, an index on it is an index on a bounded
/// column — emitted, not left out the way an index on unbounded text is.
#[test]
fn an_index_on_a_bounded_key_column_is_emitted() {
    let mut files = TEXT_KEYS.to_vec();
    files.push((
        "table/app/visits.ddl",
        "create table visits (id integer primary key, country text references countries (code), note text);\n\
         create index visits_country_idx on visits (country);\n\
         create index visits_note_idx on visits (note);",
    ));
    for dialect in [Dialect::MySql, Dialect::TSql] {
        let (sql, report) = emit_with(dialect, "", &files, None);
        let stmts = statements(&sql);
        assert!(stmts.contains("visits_country_idx"), "{dialect:?}: {sql}");
        assert!(
            !stmts.contains("visits_note_idx"),
            "{dialect:?}: still unbounded: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.from.contains("visits_note_idx") && d.to == "no index"),
            "{dialect:?}: {report:?}"
        );
    }
}

/// MySQL checks that an AUTO_INCREMENT column leads a key when the table is
/// created, and an index is a separate `CREATE INDEX` after it — too late. So
/// only a key inside the CREATE TABLE (the primary key, a UNIQUE) qualifies.
#[test]
fn mysql_numbers_only_a_column_whose_key_is_inside_the_create_table() {
    let files = [(
        "table/app/runs.ddl",
        "create table runs (id integer primary key, n bigint generated by default as identity);\n\
         create index runs_n_idx on runs (n);",
    )];
    let (sql, report) = emit_with(Dialect::MySql, "", &files, None);
    assert!(!sql.contains("AUTO_INCREMENT"), "{sql}");
    assert!(
        about(&report, "n").iter().any(|d| d.to == "a plain column"),
        "{report:?}"
    );
}

// ── A file dbd could not read is not silently missing ──────────────────────

/// An entity whose file does not parse was filtered out of the script without
/// a word: exit 0, an empty report, and a schema missing a table. It cannot be
/// emitted — there is no structure to translate — but the omission is a loss
/// like any other, and is reported.
#[test]
fn an_entity_whose_file_does_not_parse_is_reported_as_missing() {
    let files = [
        ("table/app/good.ddl", "create table good (id integer primary key);"),
        ("table/app/bad.ddl", "create table bad (id integer primary key,, oops);"),
    ];
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &files, None);
        assert!(
            report
                .iter()
                .any(|d| d.entity == "app.bad" && d.to == "nothing" && d.reason.contains("syntax error")),
            "{dialect:?}: {report:?}"
        );
        assert!(
            sql.lines().any(|l| l.starts_with("-- dbd:") && l.contains("app.bad")),
            "{dialect:?}: and the file says so: {sql}"
        );
    }
}

// ── What is not emitted is reported, in the file and in the list ───────────

/// A trigger kept beside its table — where most projects keep one — fires a
/// routine whose body does not translate, so it is skipped like the routine.
/// It was skipped without a word: no note in the file, no report entry, exit 0
/// — while the same trigger in a file of its own would have been reported.
///
/// The routine it fires is in the project too, and the two must be reported
/// the same way: one reason, written at the site in the file and in the list.
#[test]
fn a_trigger_written_in_a_table_file_is_reported_as_skipped_like_a_function() {
    let files = [
        (
            "table/app/orders.ddl",
            "create table orders (id integer primary key, updated timestamptz);\n\
             create trigger orders_touch before update on orders\n  \
               for each row execute function app.touch();",
        ),
        (
            "function/app/touch.ddl",
            "create or replace function touch() returns trigger language plpgsql as $$\n\
             begin new.updated := now(); return new; end $$;",
        ),
    ];
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &files, None);
        assert!(
            !statements(&sql).to_uppercase().contains("TRIGGER"),
            "{dialect:?}: a trigger is not emitted: {sql}"
        );
        let trigger = report
            .iter()
            .find(|d| d.from.contains("orders_touch"))
            .unwrap_or_else(|| panic!("{dialect:?}: the trigger is reported: {report:?}"));
        assert_eq!(trigger.entity, "app.orders", "{dialect:?}: on the table it fires on");
        assert_eq!(trigger.to, "nothing", "{dialect:?}");
        let function = report
            .iter()
            .find(|d| d.entity == "app.touch")
            .unwrap_or_else(|| panic!("{dialect:?}: the function is reported: {report:?}"));
        assert_eq!(
            trigger.reason, function.reason,
            "{dialect:?}: skipped like a function, so said the same way"
        );
        for skipped in ["orders_touch", "app.touch"] {
            assert!(
                sql.lines().any(|l| l.starts_with("-- dbd:") && l.contains(skipped)),
                "{dialect:?}: `{skipped}` is noted in the file too: {sql}"
            );
        }
    }
}

/// A project of `files` (each `(path under the project, contents)`) with
/// `design.yaml` written verbatim — for the tests that need a `schemas:` or
/// `target:` block [`project_with`] does not write.
fn project_from(dir: &Path, design_yaml: &str, files: &[(&str, &str)]) -> Design {
    std::fs::write(dir.join("design.yaml"), design_yaml).unwrap();
    for (path, body) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

/// Two schemas, each with a table, grants and an RLS policy file; an extension
/// and a role from `target:`, a second role from `ddl/role/`; and a scope that
/// builds `hub` alone.
fn secured_project(dir: &Path) -> Design {
    project_from(
        dir,
        "project:\n  name: p\n\nsource:\n  dialect: postgresql\n\n\
         target:\n  postgres:\n    extensions:\n      - pgcrypto\n    roles:\n      - name: reader\n\n\
         schemas:\n  - app:\n      grants:\n        reader: [usage, select]\n  \
         - hub:\n      grants:\n        reader: [usage]\n\n\
         scopes:\n  hub_only:\n    includes: [hub]\n",
        &[
            (
                "ddl/table/app/orders.ddl",
                "set search_path to app;\ncreate table orders (id integer primary key);\n",
            ),
            (
                "ddl/table/hub/nodes.ddl",
                "set search_path to hub;\ncreate table nodes (id integer primary key);\n",
            ),
            ("ddl/role/writer.ddl", "create role writer;\n"),
            (
                "policies/app/orders.sql",
                "alter table app.orders enable row level security;\n",
            ),
            (
                "policies/hub/nodes.sql",
                "alter table hub.nodes enable row level security;\n",
            ),
        ],
    )
}

/// Whether `report` has a "not emitted" entry for `entity` whose `from`
/// mentions `what`, and the script a `-- dbd:` note naming `noted`.
fn not_emitted(sql: &str, report: &[Downgrade], entity: &str, what: &str, noted: &str) -> bool {
    report
        .iter()
        .any(|d| d.entity == entity && d.from.contains(what) && d.to == "nothing" && !d.reason.is_empty())
        && sql.lines().any(|l| l.starts_with("-- dbd:") && l.contains(noted))
}

/// Extensions, roles, grants and RLS policies are part of what a PostgreSQL
/// project declares, and none of them reach the emitted script. They were
/// missing from the report too — so a schema whose security rested on a policy
/// came out with no sign anything was missing, and the report said the
/// translation lost nothing it did not list.
#[test]
fn extensions_roles_grants_and_policies_are_reported_as_not_emitted() {
    for dialect in ALL {
        let tmp = tempfile::tempdir().unwrap();
        let design = secured_project(tmp.path());
        let (sql, report) = emit_schema(&design, dialect, None).expect("emits");

        let upper = statements(&sql).to_uppercase();
        for absent in ["EXTENSION", "CREATE ROLE", "GRANT", "POLICY", "ROW LEVEL SECURITY"] {
            assert!(!upper.contains(absent), "{dialect:?}: `{absent}` is not emitted: {sql}");
        }

        assert!(
            not_emitted(&sql, &report, "pgcrypto", "extension", "pgcrypto"),
            "{dialect:?}: the extension: {report:?}\n{sql}"
        );
        for role in ["reader", "writer"] {
            assert!(
                not_emitted(&sql, &report, role, "role", role),
                "{dialect:?}: the role `{role}` — from `target:` or `ddl/role/`: {report:?}\n{sql}"
            );
        }
        for schema in ["app", "hub"] {
            assert!(
                not_emitted(&sql, &report, schema, "grant", &format!("grants on schema `{schema}`")),
                "{dialect:?}: the grants on `{schema}`, one entry per schema: {report:?}\n{sql}"
            );
        }
        for (table, file) in [
            ("app.orders", "policies/app/orders.sql"),
            ("hub.nodes", "policies/hub/nodes.sql"),
        ] {
            assert!(
                not_emitted(&sql, &report, table, file, file),
                "{dialect:?}: the policy file for `{table}`, one entry per file: {report:?}\n{sql}"
            );
        }
    }
}

/// A scope narrows what is reported missing the way it narrows what is
/// emitted: the grants and the policy of a schema the scope does not build are
/// not this script's loss.
#[test]
fn grants_and_policies_outside_the_scope_are_not_reported() {
    for dialect in ALL {
        let tmp = tempfile::tempdir().unwrap();
        let design = secured_project(tmp.path());
        let scope = design.resolve_scope(Some("hub_only"), None).expect("scope");
        let (sql, report) = emit_schema(&design, dialect, Some(&scope)).expect("emits");

        assert!(
            not_emitted(&sql, &report, "hub", "grant", "grants on schema `hub`"),
            "{dialect:?}: {report:?}"
        );
        assert!(
            not_emitted(
                &sql,
                &report,
                "hub.nodes",
                "policies/hub/nodes.sql",
                "policies/hub/nodes.sql"
            ),
            "{dialect:?}: {report:?}"
        );
        assert!(
            !report.iter().any(|d| d.entity == "app" || d.entity == "app.orders"),
            "{dialect:?}: nothing of `app` is in this script, so none of it is its loss: {report:?}"
        );
    }
}

// ── A built-in type maps to the target's own; only an enum is called one ───

/// The first word of the type `column` is declared with in the emitted
/// `CREATE TABLE`.
fn declared_type(sql: &str, dialect: Dialect, column: &str) -> String {
    let quoted = match dialect {
        Dialect::MySql => format!("`{column}` "),
        Dialect::TSql => format!("[{column}] "),
        _ => format!("\"{column}\" "),
    };
    sql.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix(quoted.as_str()))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|ty| ty.trim_end_matches(',').to_string())
        .unwrap_or_else(|| panic!("{dialect:?}: no column `{column}` in:\n{sql}"))
}

/// `char(n)`, `real`/`float4` and `time` have an equivalent on every target,
/// and fell through to the catch-all meant for a user-defined type: emitted as
/// VARCHAR(255) or nvarchar(255), and reported as "not a built-in type — if it
/// is an enum, the allowed values are no longer enforced". Neither half was
/// true. They are built in, none is an enum, and the mapping loses nothing —
/// so it is the target's own type, and not reported.
///
/// PostgreSQL writes a precision before the zone — `timestamp (3) with time
/// zone` — so reading the type up to its parenthesis took it for a plain
/// `timestamp`, and SQL Server got a `datetime2` with no offset.
///
/// A time or timestamp keeps its fractional seconds. PostgreSQL's default is
/// microseconds; MySQL's TIME and DATETIME default to whole seconds, so a bare
/// `TIME` rounded every value to the second. MySQL gets `(6)`, or the declared
/// precision; SQL Server, whose default is finer than PostgreSQL's, gets the
/// declared precision when there is one.
#[test]
fn a_built_in_type_with_an_equivalent_maps_faithfully() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, code char(3), letter character, ratio real, \
         f4 float4, opens time, opens_ms time(3), closes time without time zone, \
         stamped timestamp(3) with time zone);",
    )];
    let expected: [(Dialect, [(&str, &str); 8]); 3] = [
        (
            Dialect::MySql,
            [
                ("code", "CHAR(3)"),
                ("letter", "CHAR(1)"),
                ("ratio", "FLOAT"),
                ("f4", "FLOAT"),
                ("opens", "TIME(6)"),
                ("opens_ms", "TIME(3)"),
                ("closes", "TIME(6)"),
                ("stamped", "DATETIME(3)"),
            ],
        ),
        (
            Dialect::TSql,
            [
                ("code", "nchar(3)"),
                ("letter", "nchar(1)"),
                ("ratio", "real"),
                ("f4", "real"),
                ("opens", "time"),
                ("opens_ms", "time(3)"),
                ("closes", "time"),
                ("stamped", "datetimeoffset(3)"),
            ],
        ),
        (
            Dialect::Sqlite,
            [
                ("code", "TEXT"),
                ("letter", "TEXT"),
                ("ratio", "REAL"),
                ("f4", "REAL"),
                ("opens", "TEXT"),
                ("opens_ms", "TEXT"),
                ("closes", "TEXT"),
                ("stamped", "TEXT"),
            ],
        ),
    ];
    for (dialect, columns) in expected {
        let (sql, report) = emit_with(dialect, "", &files, None);
        for (column, ty) in columns {
            assert_eq!(
                declared_type(&sql, dialect, column),
                ty,
                "{dialect:?} `{column}`:\n{sql}"
            );
            assert!(
                about(&report, column).is_empty(),
                "{dialect:?}: `{column}` maps faithfully, so it is not reported: {:?}",
                about(&report, column)
            );
        }
    }
}

/// `timetz`, `interval`, `inet` and `cidr` have no faithful equivalent here
/// and do lose something — but what they lose is not an enum's labels, and the
/// report said it was. Each is reported with what is actually lost.
#[test]
fn a_built_in_type_without_an_equivalent_is_reported_for_what_it_loses() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, opens_tz timetz, closes_tz time with time zone, \
         opens_tz3 time(3) with time zone, wait interval, ip inet, net cidr);",
    )];
    for dialect in ALL {
        let (sql, report) = emit_with(dialect, "", &files, None);
        let mut lossy = vec![
            ("wait", "interval type"),
            ("ip", "network address"),
            ("net", "network address"),
        ];
        // SQLite keeps every date and time as text, the offset included — the
        // convention `timestamptz` → TEXT already follows unreported.
        if dialect != Dialect::Sqlite {
            lossy.extend([
                ("opens_tz", "time zone"),
                ("closes_tz", "time zone"),
                ("opens_tz3", "time zone"),
            ]);
        }
        for (column, loss) in lossy {
            let entries = about(&report, column);
            assert_eq!(
                entries.len(),
                1,
                "{dialect:?}: `{column}` is reported once: {report:?}\n{sql}"
            );
            assert!(
                entries[0].reason.contains(loss),
                "{dialect:?}: `{column}` says it loses its {loss}: {:?}",
                entries[0]
            );
            assert!(
                !entries[0].reason.contains("enum"),
                "{dialect:?}: `{column}` is not an enum, and must not be called one: {:?}",
                entries[0]
            );
        }
    }
}

/// The enum wording is kept for the one case it describes — a column whose type
/// is an enum this project declares. A type dbd simply has no mapping for is
/// reported as that, not as a possible enum.
#[test]
fn only_a_declared_enum_is_reported_as_one() {
    let files = [
        ("enum/app/status.ddl", "create type status as enum ('open', 'closed');"),
        (
            "table/app/t.ddl",
            "create table t (id integer primary key, state status, cash money);",
        ),
    ];
    for dialect in ALL {
        let (_, report) = emit_with(dialect, "", &files, None);
        let state = about(&report, "state");
        assert!(
            state.len() == 1 && state[0].reason.contains("enum") && state[0].reason.contains("allowed values"),
            "{dialect:?}: a declared enum loses its labels, and says so: {report:?}"
        );
        let cash = about(&report, "cash");
        assert!(
            cash.len() == 1 && !cash[0].reason.contains("enum"),
            "{dialect:?}: `money` is not an enum here, and is not called one: {report:?}"
        );
    }
}

// ── SQLite gets the views and checks it can run, and only those ────────────

/// Two tables in `core` and the views and checks that broke the SQLite
/// script: a view naming `core.cities` (SQLite: "view cannot reference objects
/// in database core"), a view reading that view, a CHECK using
/// `POSITION(… IN …)`, and two things SQLite has no form of — a regex CHECK
/// and `DISTINCT ON`. Plus a view reading a table the script does not create.
fn cities_project(dir: &Path) -> Design {
    let file = |body: &str| format!("set search_path to core;\n{body}\n");
    project_from(
        dir,
        "project:\n  name: p\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - core\n\n\
         external:\n  - name: auth.users\n",
        &[
            (
                "ddl/table/core/regions.ddl",
                &file("create table regions (id integer primary key, name text not null);"),
            ),
            (
                "ddl/table/core/cities.ddl",
                &file(
                    "create table cities (\n  id integer primary key\n, name text not null\n, \
                     region_id integer references regions (id)\n, \
                     code text not null check (position('-' in code) > 0)\n, \
                     slug text check (slug ~ '^[a-z]+$')\n);",
                ),
            ),
            (
                "ddl/view/core/city_names.ddl",
                &file(
                    "create view city_names as select cities.name, r.name as region \
                     from core.cities join regions r on r.id = cities.region_id;",
                ),
            ),
            (
                "ddl/view/core/named_again.ddl",
                &file("create view named_again as select name from city_names;"),
            ),
            (
                "ddl/view/core/first_per_region.ddl",
                &file(
                    "create view first_per_region as select distinct on (region_id) name \
                     from core.cities order by region_id, name;",
                ),
            ),
            (
                "ddl/view/core/user_cities.ddl",
                &file("create view user_cities as select u.id from auth.users u;"),
            ),
        ],
    )
}

fn emit_cities() -> (String, Vec<Downgrade>) {
    let tmp = tempfile::tempdir().unwrap();
    let design = cities_project(tmp.path());
    emit_schema(&design, Dialect::Sqlite, None).expect("emits")
}

const CITY_ROWS: &str = "insert into core_regions values (1, 'North');\n\
     insert into core_cities (id, name, region_id, code) values (1, 'Oslo', 1, 'NO-OS');";

/// A view names its tables the way PostgreSQL does — `core.cities`, or bare
/// under `search_path` — and SQLite has neither: the tables are `core_cities`
/// and `core_regions` there. Naming `core.cities` failed the whole script; a
/// bare `regions` applied and failed on the first read. The view now reads the
/// tables the script creates, and a view reading that view works too.
#[cfg(feature = "sqlite")]
#[test]
fn a_sqlite_view_reads_the_tables_the_script_creates() {
    let (sql, report) = emit_cities();
    let got = on_sqlite(&sql, CITY_ROWS, "select name || '/' || region from core_city_names")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, "Oslo/North");
    let got = on_sqlite(&sql, CITY_ROWS, "select name from core_named_again").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, "Oslo");
    assert!(
        report
            .iter()
            .any(|d| d.entity == "core.city_names" && d.from.contains("view body") && d.to != "nothing"),
        "a rewritten body is still reported — SQLite's semantics are not PostgreSQL's: {report:?}"
    );
}

/// `DISTINCT ON` has no SQLite form, and a view reading a table the script
/// does not create cannot be read. Either one left in failed the script or the
/// first query, so each is left out — noted where it would have been, and
/// reported — and the rest of the script applies.
#[cfg(feature = "sqlite")]
#[test]
fn a_view_sqlite_cannot_run_is_left_out_and_reported() {
    let (sql, report) = emit_cities();
    on_sqlite(&sql, CITY_ROWS, "select 'ok'").unwrap_or_else(|e| panic!("{e}"));
    for (view, emitted, why) in [
        ("core.first_per_region", "core_first_per_region", "DISTINCT ON"),
        ("core.user_cities", "core_user_cities", "auth.users"),
    ] {
        assert!(
            !statements(&sql).contains(&format!("CREATE VIEW \"{emitted}\"")),
            "`{view}` is left out: {sql}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.entity == view && d.to == "nothing" && d.reason.contains(why)),
            "`{view}` is reported, naming what SQLite cannot run ({why}): {report:?}"
        );
        assert!(
            sql.lines()
                .any(|l| l.starts_with("-- dbd:") && l.contains(view) && l.contains(why)),
            "and noted in the file: {sql}"
        );
    }
}

/// `POSITION('-' IN code)` is SQLite's `instr(code, '-')`, so the CHECK is
/// translated and still refuses what it refused. A regex has no SQLite form at
/// all — left in, the CREATE TABLE failed and took the table with it — so that
/// CHECK alone is left out and reported, and the rows it refused are accepted.
#[cfg(feature = "sqlite")]
#[test]
fn a_check_is_translated_for_sqlite_or_left_out_and_reported() {
    let (sql, report) = emit_cities();

    let refused = on_sqlite(
        &sql,
        "insert into core_cities (id, name, code) values (2, 'Bergen', 'NOBG')",
        "select 'inserted'",
    );
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("CHECK constraint failed")),
        "the translated CHECK still refuses a code without '-': {refused:?}"
    );

    let accepted = on_sqlite(
        &sql,
        "insert into core_cities (id, name, code, slug) values (2, 'Bergen', 'NO-BG', 'NOT LOWER')",
        "select slug from core_cities",
    );
    assert_eq!(accepted.as_deref(), Ok("NOT LOWER"), "the regex CHECK is gone: {sql}");
    assert!(
        report
            .iter()
            .any(|d| d.entity == "core.cities" && d.from.contains("slug ~") && d.to == "no CHECK"),
        "and leaving it out is reported: {report:?}"
    );
}

/// A view written for SQLite returns what it returned on PostgreSQL where
/// SQLite can say so. A column PostgreSQL names itself is called that — SQLite
/// would call `count(*)` `count(*)`, and a view reading `count` found nothing.
/// NULL sorts last going up, as in PostgreSQL, where SQLite puts it first. And
/// LIKE's backslash escapes, as PostgreSQL's does and SQLite's does not.
#[cfg(feature = "sqlite")]
#[test]
fn a_sqlite_view_keeps_postgres_column_names_null_order_and_like_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let file = |body: &str| format!("set search_path to core;\n{body}\n");
    let design = project_from(
        tmp.path(),
        "project:\n  name: p\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - core\n",
        &[
            (
                "ddl/table/core/items.ddl",
                &file("create table items (id integer primary key, name text, grp text not null);"),
            ),
            (
                "ddl/view/core/counts.ddl",
                &file("create view counts as select grp, count(*) from items group by grp;"),
            ),
            (
                "ddl/view/core/busy.ddl",
                &file("create view busy as select grp from counts where count > 1;"),
            ),
            (
                "ddl/view/core/ordered.ddl",
                &file("create view ordered as select name from items order by name;"),
            ),
            (
                "ddl/view/core/underscored.ddl",
                &file("create view underscored as select name from items where name like 'a\\_%';"),
            ),
        ],
    );
    let (sql, _) = emit_schema(&design, Dialect::Sqlite, None).expect("emits");
    let rows = "insert into core_items values (1, 'a_x', 'g'), (2, 'abc', 'g'), (3, null, 'h');";
    for (query, expected) in [
        ("select group_concat(grp) from core_busy", "g"),
        ("select coalesce(name, 'NULL') from core_ordered limit 1", "a_x"),
        ("select group_concat(name) from core_underscored", "a_x"),
    ] {
        let got = on_sqlite(&sql, rows, query).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got, expected, "`{query}`:\n{sql}");
    }
}

// ── json and jsonb are JSON on every target ─────────────────────────────────

/// `json` was read as `pg_catalog.json`, matched no mapping, and became
/// `VARCHAR(255)` — reported as a type dbd had no mapping for, so a document
/// longer than 255 characters no longer fit. `json` maps as `jsonb` does: to
/// MySQL's `JSON`, and to text on SQL Server and SQLite, reported there
/// because the engine no longer validates it.
///
/// MySQL's `JSON` is also reported for `json` alone. It normalizes a document
/// as `jsonb` does — sorted keys, the last of a duplicated key, no whitespace —
/// while `json` keeps the text exactly as written, so what is read back is no
/// longer what was stored.
#[test]
fn json_and_jsonb_become_the_targets_json_type() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, doc json, docs json[], bin jsonb);",
    )];
    for (dialect, ty) in [
        (Dialect::MySql, "JSON"),
        (Dialect::TSql, "nvarchar(max)"),
        (Dialect::Sqlite, "TEXT"),
    ] {
        let (sql, report) = emit_with(dialect, "", &files, None);
        for column in ["doc", "docs", "bin"] {
            assert_eq!(
                declared_type(&sql, dialect, column),
                ty,
                "{dialect:?} `{column}`:\n{sql}"
            );
        }
        assert!(
            !sql.contains("pg_catalog") && report.iter().all(|d| !d.from.contains("pg_catalog")),
            "{dialect:?}: `json` is not `pg_catalog.json`: {report:?}\n{sql}"
        );
        let doc = about(&report, "doc");
        assert_eq!(doc.len(), 1, "{dialect:?}: `json` is reported once: {report:?}");
        let bin = about(&report, "bin");
        if dialect == Dialect::MySql {
            assert!(
                doc[0].reason.contains("key") && doc[0].reason.contains("as written"),
                "MySQL's JSON normalizes the document `json` kept as written: {:?}",
                doc[0]
            );
            assert!(bin.is_empty(), "jsonb → JSON is faithful, so not reported: {bin:?}");
        } else {
            assert!(doc[0].reason.contains("validate"), "{dialect:?}: {:?}", doc[0]);
            assert_eq!(bin.len(), 1, "{dialect:?}: `jsonb` is reported as it was: {report:?}");
        }
    }
}

// ── A type the target would refuse is switched or clamped, and reported ────

/// The type each column is declared with, keyed by column, and each column's
/// report entries — for asserting a whole table at once.
fn declared_types(sql: &str, dialect: Dialect, columns: &[&str]) -> Vec<(String, String)> {
    columns
        .iter()
        .map(|c| (c.to_string(), declared_type(sql, dialect, c)))
        .collect()
}

/// Each reported column carries its note in the script as well: the report is
/// the only safeguard, and it is written twice.
fn noted_inline(sql: &str, report: &[Downgrade], column: &str) -> bool {
    !about(report, column).is_empty() && sql.contains(&format!("-- dbd: `{column}` was"))
}

/// `emit` copied a length or precision across whatever the target allows:
/// `nvarchar(5000)` (SQL Server's nvarchar holds at most 4,000), `VARCHAR(70000)`
/// (MySQL's at most 16,383 utf8mb4 characters), `DECIMAL(50,40)` (MySQL keeps
/// at most 30 digits after the point, SQL Server 38 in all) — DDL the target
/// refuses, and nothing reported. Each now becomes the target's own type that
/// holds it — its large text type, or the widest decimal it has — and the loss
/// is reported, inline and in the list. An unbounded `varchar` was silently
/// VARCHAR(255); it is the target's unbounded text, as `text` is, and loses
/// nothing. An unbounded `numeric` was silently DECIMAL(65,30)/decimal(38,10);
/// that bound is now reported. SQLite keeps a NUMERIC value as a 64-bit integer
/// or a float, so more digits than those hold are reported too.
#[test]
fn a_length_or_precision_beyond_the_target_is_switched_or_clamped_and_reported() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, wide varchar(5000), huge varchar(70000), \
         fine decimal(50,40), big numeric(40), free varchar, amount numeric, price numeric(10,2));",
    )];
    // (column, emitted type, reported, what the reason must name)
    type Expect = [(&'static str, &'static str, bool, &'static str); 7];
    let expected: [(Dialect, Expect); 3] = [
        (
            Dialect::MySql,
            [
                ("wide", "VARCHAR(5000)", false, ""),
                ("huge", "MEDIUMTEXT", true, "16,383"),
                ("fine", "DECIMAL(40,30)", true, "30"),
                ("big", "DECIMAL(40)", false, ""),
                ("free", "LONGTEXT", false, ""),
                ("amount", "DECIMAL(65,30)", true, "65"),
                ("price", "DECIMAL(10,2)", false, ""),
            ],
        ),
        (
            Dialect::TSql,
            [
                ("wide", "nvarchar(max)", true, "4,000"),
                ("huge", "nvarchar(max)", true, "4,000"),
                ("fine", "decimal(38,28)", true, "38"),
                ("big", "decimal(38,0)", true, "38"),
                ("free", "nvarchar(max)", false, ""),
                ("amount", "decimal(38,10)", true, "38"),
                ("price", "decimal(10,2)", false, ""),
            ],
        ),
        (
            Dialect::Sqlite,
            [
                ("wide", "TEXT", false, ""),
                ("huge", "TEXT", false, ""),
                ("fine", "NUMERIC", true, "15"),
                ("big", "NUMERIC", true, "64-bit"),
                ("free", "TEXT", false, ""),
                ("amount", "NUMERIC", true, "15"),
                ("price", "NUMERIC", false, ""),
            ],
        ),
    ];
    for (dialect, columns) in expected {
        let (sql, report) = emit_with(dialect, "", &files, None);
        for (column, ty, reported, names) in columns {
            assert_eq!(
                declared_type(&sql, dialect, column),
                ty,
                "{dialect:?} `{column}`:\n{sql}"
            );
            let entries = about(&report, column);
            if reported {
                assert_eq!(entries.len(), 1, "{dialect:?}: `{column}` is reported once: {report:?}");
                assert_eq!(entries[0].to, ty, "{dialect:?}: the report names what was emitted");
                assert!(
                    entries[0].reason.contains(names),
                    "{dialect:?}: `{column}` names the limit {names}: {:?}",
                    entries[0]
                );
                assert!(
                    noted_inline(&sql, &report, column),
                    "{dialect:?}: `{column}` noted inline:\n{sql}"
                );
            } else {
                assert!(
                    entries.is_empty(),
                    "{dialect:?}: `{column}` maps faithfully: {entries:?}"
                );
            }
        }
    }
}

/// PostgreSQL's `text` and `bytea` hold up to a gigabyte; MySQL's TEXT and BLOB
/// hold 65,535 bytes — as few as 16,383 utf8mb4 characters — so a longer value
/// was refused, unreported. LONGTEXT and LONGBLOB hold four gigabytes, so the
/// mapping is faithful and stays out of the report.
#[test]
fn text_and_bytea_get_a_mysql_type_as_large_as_postgres_allows() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, body text, blob bytea);",
    )];
    let (sql, report) = emit_with(Dialect::MySql, "", &files, None);
    assert_eq!(
        declared_types(&sql, Dialect::MySql, &["body", "blob"]),
        vec![("body".into(), "LONGTEXT".into()), ("blob".into(), "LONGBLOB".into())]
    );
    assert!(report.iter().all(|d| d.column.is_none()), "{report:?}");
}

/// A row of MySQL's holds at most 65,535 bytes outside its TEXT and BLOB
/// columns, counted at the character set's widest — four bytes a character in
/// utf8mb4. Four `varchar(5000)` columns need 80,008: each is legal alone, and
/// the CREATE TABLE is refused ("Row size too large"). The widest columns are
/// stored as TEXT — which counts ten bytes — until the row fits, and each is
/// reported: a value longer than its length is no longer refused. A column an
/// index reads is the last chosen, since TEXT would leave the index out.
#[test]
fn a_mysql_row_wider_than_mysql_allows_moves_its_widest_columns_to_text() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, a varchar(700), b varchar(5000), c varchar(5000), \
         d varchar(5000), e varchar(5000));\n\
         create index t_a_idx on t (a);",
    )];
    let (sql, report) = emit_with(Dialect::MySql, "", &files, None);
    let types = declared_types(&sql, Dialect::MySql, &["a", "b", "c", "d", "e"]);
    assert_eq!(types[0].1, "VARCHAR(700)", "the indexed column keeps its type:\n{sql}");
    let moved: Vec<&str> = types
        .iter()
        .filter(|(_, ty)| ty == "TEXT")
        .map(|(c, _)| c.as_str())
        .collect();
    assert_eq!(
        moved,
        vec!["b"],
        "one column is enough to fit, the first of the widest:\n{sql}"
    );
    let entries = about(&report, "b");
    assert!(
        entries.len() == 1 && entries[0].to == "TEXT" && entries[0].reason.contains("65,535"),
        "the move is reported with the limit: {report:?}"
    );
    assert!(noted_inline(&sql, &report, "b"), "{sql}");
    assert!(sql.contains("t_a_idx"), "the index stays:\n{sql}");
}

/// SQL Server keeps a row's fixed-length columns — nchar among them — within
/// 8,060 bytes; only variable-length ones may overflow it. Three `char(2000)`
/// columns are 12,000 bytes as nchar, and the CREATE TABLE is refused. The
/// widest become nvarchar, which may overflow, until the row fits — no longer
/// padded, and reported.
#[test]
fn a_sql_server_row_wider_than_sql_server_allows_moves_its_widest_nchar_to_nvarchar() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, a char(2000), b char(2000), c char(2000));",
    )];
    let (sql, report) = emit_with(Dialect::TSql, "", &files, None);
    assert_eq!(
        declared_types(&sql, Dialect::TSql, &["a", "b", "c"]),
        vec![
            ("a".into(), "nvarchar(2000)".into()),
            ("b".into(), "nchar(2000)".into()),
            ("c".into(), "nchar(2000)".into()),
        ],
        "{sql}"
    );
    let entries = about(&report, "a");
    assert!(
        entries.len() == 1 && entries[0].reason.contains("8,060") && entries[0].reason.contains("padded"),
        "{report:?}"
    );
    assert!(noted_inline(&sql, &report, "a"), "{sql}");
}

/// An unbounded `varchar` is now the target's unbounded text, so it is keyed
/// the way `text` is: bounded in a key (reported), and an ordinary index on it
/// left out (reported) — not silently cut to 255 characters for every row.
#[test]
fn an_unbounded_varchar_is_keyed_as_text_is() {
    let files = [(
        "table/app/k.ddl",
        "create table k (code varchar primary key, note varchar);\n\
         create index k_note_idx on k (note);",
    )];
    for (dialect, bounded) in [(Dialect::MySql, "VARCHAR(255)"), (Dialect::TSql, "nvarchar(450)")] {
        let (sql, report) = emit_with(dialect, "", &files, None);
        assert_eq!(declared_type(&sql, dialect, "code"), bounded, "{dialect:?}:\n{sql}");
        assert!(
            about(&report, "code").iter().any(|d| d.to == bounded),
            "{dialect:?}: the bound is reported: {report:?}"
        );
        assert!(
            report
                .iter()
                .any(|d| d.from.contains("k_note_idx") && d.to == "no index"),
            "{dialect:?}: {report:?}"
        );
        assert!(!statements(&sql).contains("CREATE INDEX"), "{dialect:?}:\n{sql}");
    }
}

/// MySQL refuses a DATETIME whose default is written at another fractional
/// precision than the column: "If a TIMESTAMP or DATETIME column definition
/// includes an explicit fractional seconds precision value anywhere, the same
/// value must be used throughout the column definition" (MySQL 8.4 manual,
/// "Automatic Initialization and Updating for TIMESTAMP and DATETIME"). The
/// column now keeps PostgreSQL's precision, so the clock it defaults to does too.
#[test]
fn a_mysql_clock_default_has_its_columns_fractional_precision() {
    let files = [(
        "table/app/t.ddl",
        "create table t (id integer primary key, a timestamptz not null default now(), \
         b timestamp(3) default current_timestamp, c timestamp default localtimestamp);",
    )];
    let (sql, _) = emit_with(Dialect::MySql, "", &files, None);
    for line in [
        "`a` DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)",
        "`b` DATETIME(3) DEFAULT CURRENT_TIMESTAMP(3)",
        "`c` DATETIME(6) DEFAULT CURRENT_TIMESTAMP(6)",
    ] {
        assert!(sql.contains(line), "{line}:\n{sql}");
    }
}
