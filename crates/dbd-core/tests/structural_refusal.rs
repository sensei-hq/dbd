//! `diff` and `reconcile` refuse any project they cannot structurally compare.
//!
//! # Why this is not just the SQLite case again
//!
//! `reconcile::raw_snapshot_from_entities` keeps only entities whose
//! `table_def.is_some()`. For a reader that produces none, desired and live
//! both reduce to nothing, the comparison succeeds trivially, and the answer is
//! **"no drift"** — against a database that may share not one table with the
//! design. "In sync" is the one answer that must never be wrong.
//!
//! That was found and fixed for SQLite (#20), where the guard asks
//! `parser == Verbatim`. But `Verbatim` is not the only reader without a
//! structured model: **T-SQL and MySQL** produce identity and references and no
//! `table_def` either, by design — they read statement heads, not column lists.
//! Both were added *after* the guard, so both walked straight past it and
//! reported a clean diff against anything.
//!
//! The guard now asks the question it always meant: does this reader produce a
//! structure to compare?

use dbd_core::Design;
use dbd_core::parser::ParserChoice;
use std::path::Path;

/// A project in `dialect`, with one table written in that dialect.
fn project(dir: &Path, dialect: &str, ddl: &str) -> std::path::PathBuf {
    std::fs::write(
        dir.join("design.yaml"),
        format!("project:\n  name: refusal\n\nsource:\n  dialect: {dialect}\n\nschemas:\n  - dbo\n"),
    )
    .unwrap();
    let t = dir.join("ddl/table/dbo");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("issues.ddl"), ddl).unwrap();
    dir.join("design.yaml")
}

/// Every dialect whose reader produces no `table_def`, and the DDL it reads.
fn structureless() -> Vec<(&'static str, &'static str)> {
    vec![
        ("tsql", "CREATE TABLE dbo.Issues (Id int NOT NULL);"),
        ("mysql", "CREATE TABLE `Issues` (`Id` INT NOT NULL);"),
        ("sqlite", "CREATE TABLE Issues (Id INTEGER NOT NULL);"),
    ]
}

#[tokio::test]
async fn diff_refuses_every_dialect_without_a_structured_model() {
    for (dialect, ddl) in structureless() {
        let tmp = tempfile::tempdir().unwrap();
        let config = project(tmp.path(), dialect, ddl);
        let design = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).expect("load");

        // An empty database that shares nothing with the design. Reporting
        // "no drift" here is the failure being guarded against.
        let target = dbd_core::connect("sqlite::memory:", "refusal").await.expect("connect");
        let scope = design.resolve_scope(None, None).expect("scope");

        let err = match design.diff_live(&*target, Some(&scope)).await {
            Ok(d) => panic!("{dialect}: diff must refuse, not report {d:?}"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("structured"), "{dialect}: the refusal must say why: {err}");
        assert!(
            err.contains(dialect),
            "{dialect}: and name the dialect responsible: {err}"
        );
    }
}

#[tokio::test]
async fn reconcile_refuses_every_dialect_without_a_structured_model() {
    for (dialect, ddl) in structureless() {
        let tmp = tempfile::tempdir().unwrap();
        let config = project(tmp.path(), dialect, ddl);
        let design = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).expect("load");
        let target = dbd_core::connect("sqlite::memory:", "refusal").await.expect("connect");

        let result = design
            .reconcile(&*target, true, true, false, None, dbd_core::design::Progress::none())
            .await;
        assert!(
            result.is_err(),
            "{dialect}: reconcile must refuse a project it cannot structurally compare"
        );
    }
}

/// The reader that *does* produce structure must keep working — a guard that
/// refused everything would pass both tests above.
#[tokio::test]
async fn postgres_is_not_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let config = project(
        tmp.path(),
        "postgresql",
        "set search_path to dbo;\ncreate table if not exists issues (id integer primary key);",
    );
    let design = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).expect("load");
    let target = dbd_core::connect("sqlite::memory:", "refusal").await.expect("connect");
    let scope = design.resolve_scope(None, None).expect("scope");

    // It may fail for other reasons against a SQLite target, but never with the
    // structural refusal — that would mean the guard had swallowed the one
    // reader it must not.
    if let Err(e) = design.diff_live(&*target, Some(&scope)).await {
        assert!(
            !e.to_string().contains("structured"),
            "the PostgreSQL reader produces a structure and must not be refused: {e}"
        );
    }
}

/// The property the guard is derived from, stated directly: which readers
/// produce a structure is a fact about the reader, not a list to keep in sync.
#[test]
fn only_the_postgres_reader_produces_a_structured_model() {
    assert!(ParserChoice::PgQuery.produces_structure());
    for choice in [ParserChoice::TSql, ParserChoice::MySql, ParserChoice::Verbatim] {
        assert!(!choice.produces_structure(), "{choice:?}");
    }
}

// ── There is no adapter for these dialects, and the error should say so ─────

/// A URL for a database dbd has no adapter for must be refused by name.
///
/// `connect` dispatched on `convex:` and `sqlite:` and fell through to
/// **Postgres for everything else**, so `mysql://…` built a `PostgresAdapter`
/// and failed with `pool timed out while waiting for an open connection` — a
/// PostgreSQL error, mentioning neither MySQL nor the absence of an adapter.
/// Reading those dialects arrived before any adapter did; the message now says
/// which half exists.
#[tokio::test]
async fn a_url_for_an_unsupported_database_names_it() {
    for (url, name) in [
        ("mysql://root@localhost:3306/shop", "MySQL"),
        ("mariadb://root@localhost:3306/shop", "MySQL"),
        ("sqlserver://sa@localhost:1433/db", "SQL Server"),
        ("mssql://sa@localhost:1433/db", "SQL Server"),
    ] {
        let err = match dbd_core::connect(url, "p").await {
            Ok(_) => panic!("{url}: must not connect"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains(name), "{url}: must name the database: {err}");
        assert!(
            !err.contains("pool timed out"),
            "{url}: must not surface a PostgreSQL connection error: {err}"
        );
        assert!(
            err.contains("read") || err.contains("parse"),
            "{url}: and should say reading it IS supported: {err}"
        );
    }
}

/// A scheme dbd genuinely treats as PostgreSQL must keep working.
#[tokio::test]
async fn a_postgres_url_still_routes_to_postgres() {
    let err = match dbd_core::connect("postgres://nobody@127.0.0.1:1/absent", "p").await {
        Ok(_) => panic!("nothing is listening on that port"),
        Err(e) => e.to_string(),
    };
    assert!(
        !err.contains("no adapter"),
        "a postgres:// URL must still reach the Postgres adapter: {err}"
    );
}
