//! A key change a foreign key holds, in a released project's migrations
//! (#65): the migration of the table whose key changes drops the foreign keys
//! holding the key first, and adds back the ones the design keeps after it.
//! On PostgreSQL a key the design left unnamed is dropped by the name the
//! catalog gives it; elsewhere the migration still says to drop it by hand.

use std::path::Path;

use dbd_core::Design;
use dbd_core::snapshot::{MigrationDialect, create_snapshot_in};

const YAML: &str = "project:\n  name: rekey\nsource:\n  dialect: postgresql\nschemas:\n  - app\n";

const PARENT_V1: &str = "create table if not exists app.parent (\n  owner_id uuid not null\n, path text not null\n, primary key (owner_id, path)\n);\n";
const CHILD_V1: &str = "create table if not exists app.child (\n  id uuid primary key\n, owner_id uuid not null\n, path text\n, foreign key (owner_id, path) references app.parent(owner_id, path) on delete cascade\n);\n";
const PARENT_V2: &str = "create table if not exists app.parent (\n  owner_id uuid not null\n, branch text not null\n, path text not null\n, primary key (owner_id, branch, path)\n);\n";
const CHILD_V2: &str = "create table if not exists app.child (\n  id uuid primary key\n, owner_id uuid not null\n, branch text not null\n, path text\n, foreign key (owner_id, branch, path) references app.parent(owner_id, branch, path) on delete cascade\n);\n";

/// Write `files` over the project in `dir` and cut the next version.
fn cut(dir: &Path, files: &[(&str, &str)], dialect: MigrationDialect) {
    std::fs::write(dir.join("design.yaml"), YAML).unwrap();
    for (table, ddl) in files {
        let path = dir.join(format!("ddl/table/app/{table}.ddl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, ddl).unwrap();
    }
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");
    create_snapshot_in(design.entities(), dir, &dir.join("design.yaml"), None, "next", dialect).expect("snapshot");
}

fn migration(dir: &Path, table: &str) -> String {
    std::fs::read_to_string(dir.join(format!("migrations/002/app/{table}.sql")))
        .unwrap_or_else(|e| panic!("migrations/002/app/{table}.sql: {e}"))
}

fn at(sql: &str, part: &str) -> usize {
    sql.find(part).unwrap_or_else(|| panic!("{part:?} not in:\n{sql}"))
}

/// The issue's repro, released: `app.parent`'s migration drops `app.child`'s
/// foreign key before its own key — both unnamed in the design, so both by
/// the names the catalog has — and `app.child`'s adds the new one.
#[test]
fn a_key_change_drops_the_foreign_keys_holding_it_first() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    cut(
        dir,
        &[("parent", PARENT_V1), ("child", CHILD_V1)],
        MigrationDialect::Postgres,
    );
    cut(
        dir,
        &[("parent", PARENT_V2), ("child", CHILD_V2)],
        MigrationDialect::Postgres,
    );
    let parent = migration(dir, "parent");
    assert!(!parent.contains("drop it manually"), "{parent}");
    let fk = at(&parent, "contype = 'f'");
    assert!(fk < at(&parent, "contype = 'p'"), "{parent}");
    assert!(
        at(&parent, "to_regclass('app.child')") < at(&parent, "contype = 'p'"),
        "{parent}"
    );
    assert!(
        at(&parent, "contype = 'p'") < at(&parent, "ADD PRIMARY KEY (owner_id, branch, path)"),
        "{parent}"
    );
    let child = migration(dir, "child");
    assert!(
        child.contains("ADD FOREIGN KEY (owner_id, branch, path) REFERENCES app.parent(owner_id, branch, path)"),
        "{child}"
    );
}

/// `app.parent` takes a surrogate key and keeps `(owner_id, path)` unique:
/// `app.child`'s foreign key is unchanged, so `app.parent`'s migration adds
/// it back once that key is there again.
#[test]
fn a_foreign_key_the_design_keeps_comes_back_after_the_new_key() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let parent_v1 = "create table if not exists app.parent (\n  id uuid not null\n, owner_id uuid not null\n, path text not null\n, primary key (owner_id, path)\n);\n";
    let parent_v2 = "create table if not exists app.parent (\n  id uuid not null\n, owner_id uuid not null\n, path text not null\n, primary key (id)\n, unique (owner_id, path)\n);\n";
    cut(
        dir,
        &[("parent", parent_v1), ("child", CHILD_V1)],
        MigrationDialect::Postgres,
    );
    cut(
        dir,
        &[("parent", parent_v2), ("child", CHILD_V1)],
        MigrationDialect::Postgres,
    );
    let parent = migration(dir, "parent");
    let drop = at(&parent, "contype = 'f'");
    assert!(drop < at(&parent, "contype = 'p'"), "{parent}");
    let add = at(
        &parent,
        "ALTER TABLE app.child ADD FOREIGN KEY (owner_id, path) REFERENCES app.parent(owner_id, path)",
    );
    assert!(at(&parent, "UNIQUE (owner_id, path)") < add, "{parent}");
    assert!(at(&parent, "PRIMARY KEY (id)") < add, "{parent}");
    assert!(!dir.join("migrations/002/app/child.sql").exists());
}

/// Where the migration is not PostgreSQL's, an unnamed key is still left to
/// be dropped by hand — but the foreign keys holding it are named first.
#[test]
fn a_portable_migration_still_leaves_an_unnamed_key_to_drop_by_hand() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    cut(
        dir,
        &[("parent", PARENT_V1), ("child", CHILD_V1)],
        MigrationDialect::Portable,
    );
    cut(
        dir,
        &[("parent", PARENT_V2), ("child", CHILD_V2)],
        MigrationDialect::Portable,
    );
    let parent = migration(dir, "parent");
    assert!(!parent.contains("DO $"), "{parent}");
    assert!(
        at(&parent, "cannot drop FOREIGN KEY (owner_id, path)")
            < at(&parent, "cannot drop PRIMARY KEY (owner_id, path)"),
        "{parent}"
    );
}

/// The migrations a database runs are its adapter's: PostgreSQL's for any URL
/// dbd's PostgreSQL adapter connects to, whatever the target is called; with
/// no URL to go by, the target's name decides.
#[test]
fn the_migration_dialect_follows_the_url() {
    use MigrationDialect::{Portable, Postgres};
    assert_eq!(MigrationDialect::of(Some("postgres://localhost/app"), "prod"), Postgres);
    assert_eq!(
        MigrationDialect::of(Some("postgresql://localhost/app"), "dev"),
        Postgres
    );
    assert_eq!(MigrationDialect::of(Some("sqlite://app.db"), "postgres"), Portable);
    assert_eq!(MigrationDialect::of(Some("file:/tmp/app.db"), "prod"), Portable);
    assert_eq!(MigrationDialect::of(Some("convex:./out"), "prod"), Portable);
    assert_eq!(MigrationDialect::of(None, "supabase"), Postgres);
    assert_eq!(MigrationDialect::of(None, "postgres"), Postgres);
    assert_eq!(MigrationDialect::of(None, "sqlite"), Portable);
}

/// The kept foreign key renamed in the same version: the holder's own
/// migration adds it under its new name, so the key's migration does not add
/// it back as well.
#[test]
fn a_kept_foreign_key_renamed_is_added_once() {
    let parent_v1 = "create table if not exists app.parent (\n  id uuid not null\n, owner_id uuid not null\n, path text not null\n, primary key (owner_id, path)\n);\n";
    let parent_v2 = "create table if not exists app.parent (\n  id uuid not null\n, owner_id uuid not null\n, path text not null\n, primary key (id)\n, unique (owner_id, path)\n);\n";
    let child = |constraint: &str| {
        format!(
            "create table if not exists app.child (\n  id uuid primary key\n, owner_id uuid not null\n, path text\n\
             , {constraint}foreign key (owner_id, path) references app.parent(owner_id, path)\n);\n"
        )
    };
    for (renamed, add) in [
        (
            "constraint child_parent_b ",
            "ADD CONSTRAINT child_parent_b FOREIGN KEY",
        ),
        ("", "ADD FOREIGN KEY (owner_id, path)"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        cut(
            dir,
            &[("parent", parent_v1), ("child", &child("constraint child_parent_a "))],
            MigrationDialect::Postgres,
        );
        cut(
            dir,
            &[("parent", parent_v2), ("child", &child(renamed))],
            MigrationDialect::Postgres,
        );
        let parent = migration(dir, "parent");
        let child_sql = migration(dir, "child");
        let adds = parent.matches(add).count() + child_sql.matches(add).count();
        assert_eq!(adds, 1, "{add}:\n{parent}\n--\n{child_sql}");
    }
}

/// A foreign key naming no columns references the primary key; added back, it
/// still names none — `REFERENCES app.parent`, not `app.parent()`.
#[test]
fn a_foreign_key_naming_no_columns_comes_back_as_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let parent = |name: &str| {
        format!(
            "create table if not exists app.parent (\n  id uuid not null\n, constraint {name} primary key (id)\n);\n"
        )
    };
    let child = "create table if not exists app.child (\n  id uuid primary key\n, parent_id uuid\n, foreign key (parent_id) references app.parent\n);\n";
    cut(
        dir,
        &[("parent", &parent("parent_id_pk")), ("child", child)],
        MigrationDialect::Postgres,
    );
    cut(
        dir,
        &[("parent", &parent("parent_pkey")), ("child", child)],
        MigrationDialect::Postgres,
    );
    let sql = migration(dir, "parent");
    assert!(
        sql.contains("REFERENCES app.parent;") && !sql.contains("app.parent()"),
        "{sql}"
    );
}

/// A unique index a foreign key rests on, renamed: the migration drops it by
/// its old name, so the foreign key goes first and comes back after.
#[test]
fn a_renamed_unique_index_a_foreign_key_rests_on_drops_it_first() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let parent = |index: &str| {
        format!(
            "set search_path to app;\ncreate table if not exists parent (\n  id integer primary key\n, code text not null\n);\n\
             create unique index if not exists {index} on parent (code);\n"
        )
    };
    let child = "set search_path to app;\ncreate table if not exists child (\n  id integer primary key\n, code text references parent (code)\n);\n";
    cut(
        dir,
        &[("parent", &parent("parent_code_uq")), ("child", child)],
        MigrationDialect::Postgres,
    );
    cut(
        dir,
        &[("parent", &parent("parent_code_key")), ("child", child)],
        MigrationDialect::Postgres,
    );
    let sql = migration(dir, "parent");
    let drop_index = at(&sql, "DROP INDEX IF EXISTS \"parent_code_uq\"");
    assert!(at(&sql, "contype = 'f'") < drop_index, "{sql}");
    assert!(drop_index < at(&sql, "ADD FOREIGN KEY (code)"), "{sql}");
}
