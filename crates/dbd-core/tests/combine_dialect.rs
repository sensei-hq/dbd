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

/// A project in `dialect` declaring schema `sales`, with each `(path under
/// ddl/, sql)` written as a file.
fn combined_files(dialect: &str, files: &[(&str, &str)]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        format!("project:\n  name: combine\n\nsource:\n  dialect: {dialect}\n\nschemas:\n  - sales\n"),
    )
    .unwrap();
    for (path, sql) in files {
        let file = dir.join("ddl").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, sql).unwrap();
    }
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    let out = dir.join("combined.sql");
    design.combine(&out, None).expect("combine");
    read(&out)
}

/// The script's batches — what sqlcmd and SSMS send one at a time: the text
/// between lines that hold only `GO`.
fn tsql_batches(script: &str) -> Vec<String> {
    let mut batches = vec![String::new()];
    for line in script.lines() {
        if line.trim().eq_ignore_ascii_case("go") {
            batches.push(String::new());
        } else {
            let batch = batches.last_mut().unwrap();
            batch.push_str(line);
            batch.push('\n');
        }
    }
    batches
}

/// `CREATE VIEW` must be the only statement in its batch on SQL Server, and
/// `combine` wrote the files back to back: the view shared a batch with the
/// table before it, and sqlcmd refused the script with "'CREATE VIEW' must be
/// the first statement in a query batch". Every entity's DDL now ends its own
/// batch — and a file that already ends in `GO` gets no second one, so the
/// script holds no empty batch.
#[test]
fn a_tsql_script_puts_every_entity_in_a_batch_of_its_own() {
    let out = combined_files(
        "tsql",
        &[
            (
                "table/sales/Orders.sql",
                "CREATE TABLE sales.Orders (Id int NOT NULL PRIMARY KEY);\n",
            ),
            (
                "table/sales/Lines.sql",
                "CREATE TABLE sales.Lines (Id int NOT NULL PRIMARY KEY);\ngo\n",
            ),
            (
                "view/sales/OpenOrders.sql",
                "CREATE VIEW sales.OpenOrders AS SELECT Id FROM sales.Orders;\n",
            ),
        ],
    );
    let batches = tsql_batches(&out);
    for statement in [
        "CREATE TABLE sales.Orders",
        "CREATE TABLE sales.Lines",
        "CREATE VIEW sales.OpenOrders",
    ] {
        let holding: Vec<&String> = batches.iter().filter(|b| b.contains(statement)).collect();
        assert_eq!(holding.len(), 1, "`{statement}` is in one batch:\n{out}");
        assert_eq!(
            holding[0].matches("CREATE ").count(),
            1,
            "`{statement}` is alone in its batch:\n{out}"
        );
    }
    assert!(
        batches.iter().take(batches.len() - 1).all(|b| !b.trim().is_empty()),
        "no empty batch between two GOs:\n{out}"
    );
}

/// `GO` is sqlcmd's batch separator, not SQL: PostgreSQL, MySQL and SQLite
/// would each fail on it.
#[test]
fn only_a_tsql_script_is_split_into_batches() {
    for (dialect, files) in [
        (
            "postgresql",
            [
                (
                    "table/sales/orders.ddl",
                    "set search_path to sales;\ncreate table if not exists orders (id int primary key);\n",
                ),
                (
                    "view/sales/open_orders.ddl",
                    "set search_path to sales;\ncreate or replace view open_orders as select id from orders;\n",
                ),
            ],
        ),
        (
            "mysql",
            [
                (
                    "table/sales/orders.sql",
                    "CREATE TABLE orders (id INT NOT NULL PRIMARY KEY);\n",
                ),
                (
                    "view/sales/open_orders.sql",
                    "CREATE VIEW open_orders AS SELECT id FROM orders;\n",
                ),
            ],
        ),
        (
            "sqlite",
            [
                (
                    "table/sales/orders.sql",
                    "CREATE TABLE orders (id INTEGER NOT NULL PRIMARY KEY);\n",
                ),
                (
                    "view/sales/open_orders.sql",
                    "CREATE VIEW open_orders AS SELECT id FROM orders;\n",
                ),
            ],
        ),
    ] {
        let out = combined_files(dialect, &files);
        assert!(
            !out.lines().any(|l| l.trim().eq_ignore_ascii_case("go")),
            "{dialect} has no batch separator: {out}"
        );
    }
}
