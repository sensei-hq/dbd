//! Which schemas are reachable from outside, and what that implies (#7).
//!
//! # Why dbd needs the distinction
//!
//! Supabase exposes some schemas through PostgREST and keeps the rest private.
//! A table in an exposed schema is reachable by `anon` over HTTP; a table in a
//! private one is not. dbd had no way to say which was which, so it could not
//! answer the question that actually matters on a Supabase project: **is this
//! table readable by the internet, and does it have a policy?**
//!
//! `SUPABASE_PROTECTED` was the closest thing, and it is a different axis. It
//! conflates "Supabase owns this" (`auth`, `vault`) with "PostgREST serves
//! this" (`public`) — `extensions` is protected and not exposed, and a
//! project's own `app` schema can be exposed without being Supabase's.
//! Exposure is a property of the project's intent, not of the platform.
//!
//! # The default mirrors Supabase
//!
//! Internal unless declared otherwise, except that `public` is exposed by
//! default **on a `supabase` target** — which is what Supabase itself does. On
//! a plain `postgres` target nothing serves HTTP, so nothing is exposed.

use dbd_core::Design;
use std::path::Path;

/// A project with an exposed schema, a private one, and a table in each.
fn project(dir: &Path, target: &str, schemas: &str) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        format!(
            "project:\n  name: exposure\n\nsource:\n  dialect: postgresql\n\
             target:\n  {target}:\n    url: $DATABASE_URL\n\nschemas:\n{schemas}"
        ),
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn table(dir: &Path, schema: &str, name: &str) {
    let d = dir.join("ddl/table").join(schema);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join(format!("{name}.ddl")),
        format!("set search_path to {schema};\ncreate table if not exists {name} (id integer primary key);"),
    )
    .unwrap();
}

fn policy(dir: &Path, schema: &str, name: &str) {
    let d = dir.join("policies").join(schema);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join(format!("{name}.sql")),
        format!("alter table {schema}.{name} enable row level security;"),
    )
    .unwrap();
}

// ── Declaring exposure ──────────────────────────────────────────────────────

#[test]
fn a_schema_is_internal_unless_it_says_otherwise() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "postgres", "  - app\n  - config\n");
    assert!(d.exposed_schemas().is_empty(), "nothing is exposed by default");
}

#[test]
fn a_schema_can_declare_itself_exposed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "postgres", "  - app:\n      exposed: true\n  - config\n");
    assert_eq!(d.exposed_schemas(), vec!["app".to_string()]);
}

/// The Supabase default, mirrored: PostgREST serves `public` out of the box.
#[test]
fn public_is_exposed_by_default_on_supabase() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "supabase", "  - public\n  - internal\n");
    assert_eq!(d.exposed_schemas(), vec!["public".to_string()]);
}

/// …and not on a plain Postgres target, where nothing serves HTTP.
#[test]
fn public_is_not_exposed_on_a_plain_postgres_target() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "postgres", "  - public\n");
    assert!(d.exposed_schemas().is_empty());
}

/// An explicit `exposed: false` beats the Supabase default — a project that
/// has locked `public` down should be able to say so.
#[test]
fn an_explicit_declaration_beats_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "supabase", "  - public:\n      exposed: false\n");
    assert!(
        d.exposed_schemas().is_empty(),
        "the project overrode the platform default"
    );
}

/// Exposure and platform ownership are different axes, and conflating them is
/// what made `SUPABASE_PROTECTED` unable to answer this.
#[test]
fn exposure_is_not_the_same_as_platform_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(tmp.path(), "supabase", "  - app:\n      exposed: true\n");
    assert_eq!(
        d.exposed_schemas(),
        vec!["app".to_string()],
        "a project's own schema can be exposed without being Supabase's"
    );
    assert!(
        !dbd_core::script::SUPABASE_INFRASTRUCTURE.contains(&"app"),
        "and it is not infrastructure"
    );
}

// ── What the distinction is for ─────────────────────────────────────────────

/// The payoff: a table reachable over HTTP with no RLS policy declared.
#[test]
fn a_table_in_an_exposed_schema_without_a_policy_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    table(dir, "app", "orders");
    let d = project(dir, "supabase", "  - app:\n      exposed: true\n");

    let found = d.unprotected_exposed_tables();
    assert_eq!(
        found.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        vec!["app.orders"],
        "an exposed table with no policy file must be reported"
    );
}

#[test]
fn a_table_with_a_policy_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    table(dir, "app", "orders");
    policy(dir, "app", "orders");
    let d = project(dir, "supabase", "  - app:\n      exposed: true\n");
    assert!(
        d.unprotected_exposed_tables().is_empty(),
        "a declared policy is what the check is looking for"
    );
}

/// A private schema needs no policy — reporting one would be noise, and noise
/// is how a security report stops being read.
#[test]
fn a_table_in_an_internal_schema_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    table(dir, "internal", "secrets");
    let d = project(dir, "supabase", "  - internal\n");
    assert!(d.unprotected_exposed_tables().is_empty());
}

/// And nothing is reported on a target that serves no HTTP at all.
#[test]
fn nothing_is_reported_on_a_plain_postgres_target() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    table(dir, "public", "orders");
    let d = project(dir, "postgres", "  - public\n");
    assert!(d.unprotected_exposed_tables().is_empty());
}
