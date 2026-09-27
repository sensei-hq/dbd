//! `reconcile --prune` must not drop objects in a schema the project does not
//! own (#7).
//!
//! # Why prune, specifically
//!
//! `reset` and `prune` look similar and are not. **Reset drops only what the
//! design declares** — `build_reset_script` emits a `DROP` per declared entity,
//! and its protection applies to the *schema object* alone. **Prune drops what
//! the design does NOT declare**: orphans found in the live database within the
//! managed schemas. It is the one operation that can destroy something the
//! project never knew about, and it had no protection at all.
//!
//! On a Supabase target that matters. If a design declares anything in `auth`
//! — one table is enough — `auth` joins `managed_schemas`, every other table
//! Supabase keeps there reads as an orphan, and prune drops the lot. Recreating
//! them loses the policies and grants that make the environment work.
//!
//! # The Supabase list was two lists
//!
//! `SUPABASE_PROTECTED` conflated two different properties:
//!
//! - **infrastructure** (`auth`, `storage`, `vault`, …) — Supabase owns these
//!   and the project owns nothing in them, so prune must never touch their
//!   contents;
//! - **`public`** — protected from `DROP SCHEMA` because recreating it loses
//!   its grants, but its *contents* are the project's. Reset drops entities in
//!   `public`, and prune should keep pruning it.
//!
//! One list could not express that, so it is now two.

use dbd_core::script::{SUPABASE_INFRASTRUCTURE, prune_is_forbidden_in};

// ── The property, stated directly ───────────────────────────────────────────

#[test]
fn prune_never_touches_supabase_infrastructure() {
    for schema in SUPABASE_INFRASTRUCTURE {
        assert!(
            prune_is_forbidden_in(schema, "supabase"),
            "`{schema}` is Supabase's, not the project's"
        );
    }
}

/// The distinction the single list could not make. `public` is protected from
/// `DROP SCHEMA` on Supabase — recreating it loses grants and policies — but
/// its contents belong to the project, and reset already drops them.
#[test]
fn public_is_prunable_even_on_supabase() {
    assert!(
        !prune_is_forbidden_in("public", "supabase"),
        "a Supabase project owns its own public tables"
    );
    assert!(!prune_is_forbidden_in("public", "postgres"));
}

/// Postgres internals and dbd's own bookkeeping are off limits on every
/// target, not just Supabase.
#[test]
fn the_system_schemas_are_forbidden_everywhere() {
    for target in ["postgres", "supabase"] {
        for schema in ["pg_catalog", "information_schema", "pg_toast", "dbd"] {
            assert!(prune_is_forbidden_in(schema, target), "{schema} on {target}");
        }
    }
}

/// A plain Postgres target has no Supabase infrastructure — a schema called
/// `auth` there is an ordinary project schema.
#[test]
fn supabase_infrastructure_is_only_special_on_supabase() {
    assert!(!prune_is_forbidden_in("auth", "postgres"));
    assert!(!prune_is_forbidden_in("storage", "postgres"));
    assert!(prune_is_forbidden_in("auth", "supabase"));
}

/// An ordinary project schema is prunable on both.
#[test]
fn a_project_schema_is_prunable() {
    for target in ["postgres", "supabase"] {
        assert!(!prune_is_forbidden_in("app", target));
        assert!(!prune_is_forbidden_in("config", target));
    }
}

// ── The two lists stay consistent ───────────────────────────────────────────

/// Splitting one list into two invites them drifting apart. The protected set
/// is exactly the infrastructure set plus `public`, and that is checked rather
/// than maintained by hand.
#[test]
fn the_protected_set_is_the_infrastructure_set_plus_public() {
    use dbd_core::script::SUPABASE_PROTECTED;
    let mut expected: Vec<&str> = SUPABASE_INFRASTRUCTURE.to_vec();
    expected.push("public");
    expected.sort_unstable();

    let mut actual: Vec<&str> = SUPABASE_PROTECTED.to_vec();
    actual.sort_unstable();

    assert_eq!(actual, expected, "the two lists have drifted");
}

#[test]
fn public_is_not_in_the_infrastructure_set() {
    assert!(
        !SUPABASE_INFRASTRUCTURE.contains(&"public"),
        "public is the project's; putting it here would stop prune working on Supabase"
    );
}
