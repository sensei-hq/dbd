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
/// PostgreSQL literal does not.
#[test]
fn mysql_gets_a_text_default_as_an_expression_and_its_backslashes_escaped() {
    let (mysql, _) = emit_with(Dialect::MySql, "", &DEFAULTS, None);
    assert!(mysql.contains("`status` TEXT NOT NULL DEFAULT ('open')"), "{mysql}");
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
const SERIAL: [(&str, &str); 1] = [("table/app/notes.ddl", "create table notes (id serial, body text);")];

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
