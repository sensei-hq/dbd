//! Facts the docs state about dbd, checked against dbd.
//!
//! Prose drifts because nothing executes it. These are the statements that are
//! *mechanically* checkable — a list of accepted values, a copy of a file, a
//! mapping from one name to another — so there is no reason for them to drift
//! and no excuse when they do.
//!
//! What this deliberately does **not** do is assert on wording. A test that
//! pins a sentence breaks on a harmless rewrite and teaches people to delete
//! tests. Each check here is about a fact with exactly one right answer.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The skill ships twice: once for readers, once embedded in the binary for
/// `dbd init` to write out. They are the same document and must stay identical.
///
/// Diffed by hand three times in one session before this existed, which is
/// exactly how long that lasts.
#[test]
fn the_two_skill_copies_are_identical() {
    let docs = read("docs/skills/dbd/SKILL.md");
    let embedded = read("src/assets/skills/dbd/SKILL.md");
    assert_eq!(
        docs, embedded,
        "\n\ndocs/skills/dbd/SKILL.md and src/assets/skills/dbd/SKILL.md have diverged.\n\
         The embedded copy is what `dbd init` writes into a user's project, so a\n\
         reader and a user would be given different instructions.\n\n\
         Sync with:\n\
         \x20   cp docs/skills/dbd/SKILL.md src/assets/skills/dbd/SKILL.md\n"
    );
}

/// `source.parser` accepts a fixed set of values. The guide lists them; the
/// resolver decides them. A value in one and not the other is either a
/// documented option that does not work, or a working option nobody knows
/// about.
#[test]
fn every_documented_parser_value_is_accepted() {
    use dbd_core::parser::ParserChoice;

    let guide = read("docs/guide/03-design-yaml.md");
    for value in ["pg_query", "tsql", "mysql", "verbatim"] {
        assert!(
            guide.contains(&format!("`{value}`")),
            "the guide does not mention `{value}`, which `source.parser` accepts"
        );
        assert!(
            ParserChoice::resolve("postgresql", Some(value)).is_ok(),
            "the guide lists `{value}` but the resolver rejects it"
        );
    }

    // And the retired one is still refused, with the guide still saying so.
    assert!(ParserChoice::resolve("postgresql", Some("sqlparser")).is_err());
    assert!(
        guide.contains("sqlparser"),
        "the guide should still explain what happened to `sqlparser` — a project \
         carrying it needs to find out why it stopped loading"
    );
}

/// The guide's dialect table says which reader each dialect selects. That is a
/// mapping with one right answer, and `for_dialect_typed` owns it.
#[test]
fn the_documented_dialect_table_matches_the_code() {
    use dbd_core::parser::{Dialect, ParserChoice};

    let guide = read("docs/guide/03-design-yaml.md");
    for (label, expected) in [
        ("postgresql", ParserChoice::PgQuery),
        ("supabase", ParserChoice::PgQuery),
        ("tsql", ParserChoice::TSql),
        ("mssql", ParserChoice::TSql),
        ("sqlserver", ParserChoice::TSql),
        ("mysql", ParserChoice::MySql),
        ("mariadb", ParserChoice::MySql),
        ("sqlite", ParserChoice::Verbatim),
    ] {
        assert!(
            guide.contains(label),
            "the guide's dialect table does not list `{label}`, which is accepted"
        );
        let dialect = Dialect::from_label(label)
            .unwrap_or_else(|| panic!("`{label}` is documented but `from_label` does not know it"));
        assert_eq!(
            ParserChoice::for_dialect_typed(dialect),
            expected,
            "`{label}` selects a different reader than the guide's table says"
        );
    }
}

/// Only a structured reader can be diffed, and the guide says so. If a reader
/// gained structure — or lost it — the sentence would be wrong.
///
/// Checked against the readers themselves rather than the sentence: a reader
/// that produces no `table_def` cannot be diffed, whatever any document says.
#[test]
fn the_readers_without_structure_are_the_ones_the_guide_names() {
    use dbd_core::parser::{Dialect, parse_sql_as};

    // A table, written the way each dialect writes one.
    for (dialect, sql) in [
        (Dialect::TSql, "CREATE TABLE dbo.T (Id int NOT NULL)"),
        (Dialect::MySql, "CREATE TABLE t (id INT NOT NULL);"),
    ] {
        let parsed = parse_sql_as(dialect, sql).expect("reads");
        assert!(
            parsed.entities.first().is_some_and(|e| e.table_def.is_none()),
            "{dialect:?} produced a table_def — if that is now true, the guide's \
             claim that it cannot be diffed is stale and `reconcile` should be \
             reconsidered for it"
        );
    }

    // And PostgreSQL does produce one, which is why it can be.
    let pg = parse_sql_as(Dialect::PostgreSql, "create table app.t (id int not null);").expect("reads");
    assert!(
        pg.entities[0].table_def.is_some(),
        "the PostgreSQL reader must keep producing structure — `diff` and \
         `reconcile` are built on it"
    );
}

/// `dbd init` scaffolds a folder per entity type. The guide documents the
/// layout, and a type folder the guide omits is one users will not create.
#[test]
fn every_scaffolded_ddl_folder_is_documented() {
    use dbd_core::entity::EntityType;

    // The layout is described across the guide and the README rather than on
    // one page, so the check is "documented somewhere a reader will look",
    // not "documented here".
    let docs = [
        "README.md",
        "docs/guide/01-what-is-dbd.md",
        "docs/guide/02-getting-started.md",
    ]
    .iter()
    .map(|f| read(f))
    .collect::<Vec<_>>()
    .join("\n");

    for t in [
        EntityType::Table,
        EntityType::View,
        EntityType::MaterializedView,
        EntityType::Function,
        EntityType::Procedure,
        EntityType::Enum,
    ] {
        let folder = t.folder_name();
        assert!(
            docs.contains(&folder),
            "`ddl/{folder}/` is a real entity folder but the guide never names it"
        );
    }
}
