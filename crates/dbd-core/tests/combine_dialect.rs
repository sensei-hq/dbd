//! `combine` writes a project's own DDL as one script for the project's own
//! engine — so every statement it generates must be that engine's SQL.
//!
//! The authored files are copied verbatim, and are already in the project's
//! dialect. What `combine` adds itself — a statement per declared schema — was
//! always PostgreSQL's `CREATE SCHEMA IF NOT EXISTS "x";`, whatever
//! `source.dialect` said. SQL Server has no `IF NOT EXISTS` there, and MySQL
//! and SQLite have no schemas to create, so the script failed on its first
//! line in every engine but the one it was not written for.

use dbd_core::Design;
use std::path::Path;

/// A project in `dialect` declaring schema `sales`, with one table file.
fn combined(dialect: &str, ddl: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        format!("project:\n  name: combine\n\nsource:\n  dialect: {dialect}\n\nschemas:\n  - sales\n"),
    )
    .unwrap();
    let t = dir.join("ddl/table/sales");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("orders.sql"), ddl).unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    let out = dir.join("combined.sql");
    design.combine(&out, None).expect("combine");
    read(&out)
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

const POSTGRES_CREATE_SCHEMA: &str = "CREATE SCHEMA IF NOT EXISTS \"sales\";";

#[test]
fn a_tsql_script_creates_its_schema_the_way_sql_server_does() {
    let out = combined("tsql", "CREATE TABLE sales.Orders (Id int NOT NULL PRIMARY KEY);\nGO\n");
    assert!(!out.contains(POSTGRES_CREATE_SCHEMA), "no PostgreSQL statement: {out}");
    assert!(
        out.contains("IF SCHEMA_ID(N'sales') IS NULL EXEC(N'CREATE SCHEMA [sales]');"),
        "SQL Server's idempotent form — CREATE SCHEMA has no IF NOT EXISTS there: {out}"
    );
    assert!(
        out.contains("CREATE TABLE sales.Orders"),
        "the authored DDL is kept: {out}"
    );
}

#[test]
fn a_mysql_or_sqlite_script_creates_no_schema() {
    for (dialect, ddl) in [
        ("mysql", "CREATE TABLE `orders` (`id` INT NOT NULL PRIMARY KEY);\n"),
        ("sqlite", "CREATE TABLE orders (id INTEGER NOT NULL PRIMARY KEY);\n"),
    ] {
        let out = combined(dialect, ddl);
        assert!(
            !out.to_uppercase().contains("CREATE SCHEMA"),
            "{dialect} has no schemas to create: {out}"
        );
        assert!(
            out.contains("CREATE TABLE"),
            "{dialect}: the authored DDL is kept: {out}"
        );
    }
}

/// The engine the generated statements were written for keeps them.
#[test]
fn a_postgres_script_still_creates_its_schema() {
    let out = combined(
        "postgresql",
        "set search_path to sales;\ncreate table if not exists orders (id integer primary key);\n",
    );
    assert!(out.contains(POSTGRES_CREATE_SCHEMA), "{out}");
}

/// An extension exists only in PostgreSQL. Another engine's script says so
/// instead of carrying `CREATE EXTENSION`, which that engine would reject.
#[test]
fn another_engines_script_names_a_postgres_only_extension_instead_of_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: combine\n\nsource:\n  dialect: tsql\n\n\
         target:\n  postgres:\n    extensions:\n      - pgcrypto\n\nschemas:\n  - sales\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    let out = dir.join("combined.sql");
    design.combine(&out, None).expect("combine");
    let out = read(&out);

    assert!(!out.contains("CREATE EXTENSION"), "{out}");
    assert!(
        out.contains("pgcrypto is PostgreSQL-only"),
        "the omission is stated: {out}"
    );
}
