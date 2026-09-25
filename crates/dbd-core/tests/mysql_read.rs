//! Reading MySQL.
//!
//! The same statement-head walk as T-SQL under different rules, because the
//! walk — find a head, read the name after it — is what every SQL dialect has
//! in common. What differs is small, specific, and would be wrong the other
//! way round:
//!
//! - **`ALTER` never carries a body.** MySQL's `ALTER PROCEDURE` changes
//!   characteristics (`COMMENT`, `SQL SECURITY`) and nothing else; a body
//!   change needs `DROP` then `CREATE`. T-SQL's carries the whole thing.
//! - **`a.b` is `database.object`.** MySQL has no schemas, so the first part is
//!   the database — which in dbd's model is the catalog, not the schema.
//! - **Backticks quote and `#` comments.** In T-SQL a backtick is nothing and
//!   `#temp` is a name.
//!
//! **Fixture-verified only.** The T-SQL reader was measured against 2,154 real
//! files; no MySQL corpus was available. These tests prove the reader handles
//! the cases their author thought of, which is not the same as evidence — the
//! `#[ignore]`d corpus gate in `tsql_corpus.rs` is pointed at `DBD_SQL_CORPUS`
//! and will measure this too when one turns up.

use dbd_core::entity::EntityType;
use dbd_core::parser::{Dialect, FileKind, parse_sql_as};

fn read(sql: &str) -> dbd_core::parser::ParsedFile {
    parse_sql_as(Dialect::MySql, sql).expect("the MySQL reader does not fail on input")
}

fn names(p: &dbd_core::parser::ParsedFile) -> Vec<String> {
    p.entities.iter().map(|e| e.name.clone()).collect()
}

// ── Declarations ────────────────────────────────────────────────────────────

#[test]
fn a_backtick_quoted_table_is_declared() {
    let p = read("CREATE TABLE `order` (`id` INT AUTO_INCREMENT PRIMARY KEY) ENGINE=InnoDB;");
    assert_eq!(names(&p), vec!["order"]);
    assert_eq!(p.entities[0].entity_type, EntityType::Table);
    assert_eq!(p.kind, FileKind::Declaration);
}

#[test]
fn each_statement_head_declares_the_kind_it_names() {
    for (sql, expected) in [
        ("CREATE TABLE t (id INT)", EntityType::Table),
        ("CREATE VIEW v AS SELECT 1", EntityType::View),
        ("CREATE PROCEDURE p() BEGIN SELECT 1; END", EntityType::Procedure),
        ("CREATE FUNCTION f() RETURNS INT RETURN 1", EntityType::Function),
        (
            "CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW SET @x = 1",
            EntityType::Trigger,
        ),
    ] {
        assert_eq!(
            read(sql).entities.first().map(|e| e.entity_type),
            Some(expected),
            "{sql}"
        );
    }
}

// ── The database is the catalog, not a schema ───────────────────────────────

/// MySQL has no schemas. Reading `shop.users` as `schema.object` would put two
/// databases' tables in one namespace, which is the merge `Entity::catalog`
/// exists to prevent.
#[test]
fn a_two_part_name_puts_the_database_in_the_catalog() {
    let p = read("CREATE TABLE shop.users (id INT);");
    let e = &p.entities[0];
    assert_eq!(e.catalog.as_deref(), Some("shop"), "the database is the catalog");
    assert_eq!(e.schema, None, "MySQL has no schemas");
    assert_eq!(e.name, "users", "the name is the bare object");
    assert_eq!(e.qualified_key(), "shop.users");
}

/// The same table name in two databases must stay two entities.
#[test]
fn the_same_table_in_two_databases_is_two_entities() {
    let p = read("CREATE TABLE shop.users (id INT);\nCREATE TABLE crm.users (id INT);");
    assert_eq!(p.entities.len(), 2);
    let keys: Vec<String> = p.entities.iter().map(|e| e.qualified_key()).collect();
    assert_eq!(keys, vec!["shop.users", "crm.users"]);
}

/// An unqualified name has no database — which database it lands in depends on
/// the connection's `USE`, and no file states that.
#[test]
fn an_unqualified_name_carries_no_catalog() {
    let p = read("CREATE TABLE users (id INT);");
    assert_eq!(p.entities[0].catalog, None);
    assert_eq!(p.entities[0].name, "users");
}

// ── ALTER never declares ────────────────────────────────────────────────────

/// MySQL's `ALTER PROCEDURE` changes characteristics only — a body change needs
/// `DROP` then `CREATE`. So unlike T-SQL, it refers rather than declares, and
/// reading it the T-SQL way would mint a procedure from a comment change.
#[test]
fn alter_procedure_refers_because_mysql_alter_carries_no_body() {
    let p = read("ALTER PROCEDURE p COMMENT 'now documented';");
    assert!(p.entities.is_empty(), "declared {:?}", names(&p));
    assert_eq!(p.kind, FileKind::Migration);
}

#[test]
fn alter_table_refers_as_it_does_everywhere() {
    let p = read("ALTER TABLE users ADD COLUMN email VARCHAR(255);");
    assert!(p.entities.is_empty());
    assert_eq!(p.kind, FileKind::Migration);
}

/// The redeploy idiom works the same way here: the DROP names what the file
/// declares, so it is not a change to something else.
#[test]
fn drop_then_create_is_a_declaration_not_a_change() {
    let p = read("DROP PROCEDURE IF EXISTS p;\nCREATE PROCEDURE p() BEGIN SELECT 1; END");
    assert_eq!(names(&p), vec!["p"]);
    assert_eq!(p.kind, FileKind::Declaration);
}

// ── References ──────────────────────────────────────────────────────────────

#[test]
fn reads_and_writes_are_kept_apart() {
    let p = read(
        "CREATE PROCEDURE sync() BEGIN\n\
           INSERT INTO target SELECT * FROM source;\n\
           UPDATE audit SET n = 1;\n\
         END",
    );
    let e = &p.entities[0];
    assert!(e.reads.contains(&"source".to_string()), "reads: {:?}", e.reads);
    assert!(e.writes.contains(&"target".to_string()), "writes: {:?}", e.writes);
    assert!(e.writes.contains(&"audit".to_string()), "writes: {:?}", e.writes);
}

#[test]
fn a_foreign_key_names_the_table_it_points_at() {
    let p = read("CREATE TABLE orders (user_id INT, FOREIGN KEY (user_id) REFERENCES users(id));");
    assert!(
        p.entities[0].refers.contains(&"users".to_string()),
        "{:?}",
        p.entities[0].refers
    );
}

/// A cross-database read keeps the database it named.
#[test]
fn a_cross_database_reference_keeps_its_database() {
    let p = read("CREATE VIEW v AS SELECT * FROM crm.contacts;");
    assert_eq!(p.entities[0].reads, vec!["crm.contacts"]);
}

// ── `#` is a comment here ───────────────────────────────────────────────────

/// The direct conflict with T-SQL, where `#temp` is a name. Read the MySQL way
/// in a T-SQL file and every temp table swallows its line; read the T-SQL way
/// here and every comment becomes a phantom table.
#[test]
fn a_hash_comment_is_not_read_as_sql() {
    let p = read("# CREATE TABLE ghost (id INT);\nCREATE TABLE real_t (id INT);");
    assert_eq!(names(&p), vec!["real_t"], "the commented declaration is not one");
}

#[test]
fn a_dash_comment_works_too() {
    let p = read("-- CREATE TABLE ghost (id INT);\nCREATE TABLE real_t (id INT);");
    assert_eq!(names(&p), vec!["real_t"]);
}

// ── Nothing invented ────────────────────────────────────────────────────────

#[test]
fn a_pure_data_script_declares_nothing() {
    let p = read("INSERT INTO lookup (id, name) VALUES (1, 'a');");
    assert!(p.entities.is_empty());
    assert_eq!(p.kind, FileKind::Data);
}

#[test]
fn the_dialect_is_reported_and_no_table_def_is_claimed() {
    let p = read("CREATE TABLE t (id INT NOT NULL, name VARCHAR(50));");
    assert_eq!(p.dialect, Dialect::MySql);
    assert!(
        p.entities[0].table_def.is_none(),
        "a statement-head reader has no columns to offer"
    );
}

/// `DELIMITER` is a client directive, not SQL — the same kind of thing as
/// T-SQL's `GO`. It must not be mistaken for a declaration or swallow one.
#[test]
fn a_delimiter_directive_does_not_break_the_read() {
    let p = read(
        "DELIMITER $$\n\
         CREATE PROCEDURE p() BEGIN SELECT * FROM t; END$$\n\
         DELIMITER ;",
    );
    assert_eq!(names(&p), vec!["p"]);
    assert!(
        p.entities[0].reads.contains(&"t".to_string()),
        "{:?}",
        p.entities[0].reads
    );
}
