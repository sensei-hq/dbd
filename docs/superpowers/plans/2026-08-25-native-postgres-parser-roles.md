# Native Postgres Parser — Role Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `EntityType::Role` onto libpg_query and delete `extract_role_memberships`, the last regex scanner in the parser.

**Architecture:** `PgQueryDdl::native()` gains a `Role` arm calling a new `pg/roles.rs`, which reads `GrantRoleStmt` nodes from libpg_query's AST. The `parse_entity` early-return for Role, and the regex it calls, both go away.

**Tech Stack:** Rust 2024, `pg_query` 6.

**Spec:** `docs/superpowers/specs/2026-08-24-native-postgres-parser-design.md` (rollout step 4)

**Prior increments:** enum (`2026-08-24-native-postgres-parser.md`), view (`2026-08-25-native-postgres-parser-views.md`), function/procedure (`2026-08-25-native-postgres-parser-procs.md`).

---

## Why this is one task

Under `clippy --all-targets -- -D warnings`, a `parse_role` nothing dispatches to is dead code, and neither `#[cfg(test)]` callers nor integration tests clear it. Parser, corpus and switchover land together. This boundary has been hit three times in this migration; do not re-split it.

---

## This is a strict improvement, not just a port

Role DDL is idempotent-wrapped — `DO $$ … CREATE ROLE … END $$;` — which sqlparser cannot parse at all. So `parse_entity` special-cases Role with an early return that scans the raw text with a regex:

```rust
if entity.entity_type == EntityType::Role {
    extract_role_memberships(sql, &mut entity);
    return Ok(entity);
}
```

That regex must distinguish a **role grant** (`GRANT parent TO member`) from an **object grant** (`GRANT SELECT ON TABLE t TO member`). It does so by requiring `TO` to follow the identifier immediately, which is a fragile lookahead over text.

libpg_query makes this a **type distinction**. Measured:

| input | parses | node |
| --- | --- | --- |
| `GRANT "app_admin" TO "app_ro";` | ✅ | `GrantRoleStmt` |
| `grant parent to child;` (bare idents) | ✅ | `GrantRoleStmt` |
| `GRANT SELECT ON TABLE t TO app_ro;` | ✅ | **`GrantStmt`** — a different node |
| `grant a to c with admin option;` | ✅ | `GrantRoleStmt` |
| two grants in one file | ✅ | two `GrantRoleStmt` |

Reading only `GrantRoleStmt` excludes object grants structurally. The `DO $$ … $$` wrapper parses as a `DoStmt` alongside it and is simply ignored.

Node shape: `GrantRoleStmt.granted_roles` holds `AccessPriv { priv_name }` (the parent), `grantee_roles` holds `RoleSpec { rolename }` (the member).

---

## The parity contract — read this before writing code

Role's `Entity` shape differs from every other native type, because the early return skips the shared code that runs after sqlparser. Measured on the emitted form plus a trailing object grant:

```
name          = "app_ro"
schema        = None
search_paths  = []                       <-- NOT ["public"]
refers        = ["app_admin"]
references    = [("app_admin", ref_type: None)]
reads/writes  = []
errors        = []
```

Two traps:

1. **`search_paths` must stay empty.** Every other native parser sets `["public"]` when the file has no `set search_path`, because references are qualified against it. Role does not qualify anything — a role name has no schema — and the incumbent never reaches the search-path extraction. Setting it would fail the gate.
2. **`ref_type` is `None`, not `REF_TYPE_FUNCTION`.** A role membership is a hard reference.

Also note role names are **not schema-qualified**: `refers` holds `"app_admin"`, not `"public.app_admin"`. Do not pass these through `qualify_name_str`.

---

## Task 1: Native role parsing, corpus and switchover

**Files:**
- Create: `crates/dbd-core/src/parser/pg/roles.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`
- Modify: `crates/dbd-core/src/parser/mod.rs` — delete the Role early return and `extract_role_memberships`
- Create: `tests/fixtures/parser_corpus/ddl/role/*.ddl`

- [ ] **Step 1: Write the failing tests**

Create `crates/dbd-core/src/parser/pg/roles.rs` with ONLY this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        parse_role(Entity::new(EntityType::Role, "app_ro"), sql).unwrap()
    }

    /// The form `dbd` itself emits (`script::generate_role_script`), so this is
    /// the round-trip case.
    #[test]
    fn emitted_form_yields_its_memberships() {
        let e = parse(
            "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_ro') THEN\n    CREATE ROLE \"app_ro\";\n  END IF;\nEND $$;\nGRANT \"app_admin\" TO \"app_ro\";\n",
        );
        assert_eq!(e.refers, vec!["app_admin".to_string()]);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn bare_identifiers_are_handled() {
        let e = parse("grant parent to child;");
        assert_eq!(e.refers, vec!["parent".to_string()]);
    }

    /// An object grant is a different statement type in Postgres's grammar, so
    /// this exclusion is structural rather than a text-matching lookahead.
    #[test]
    fn object_grants_are_not_memberships() {
        let e = parse("GRANT SELECT ON TABLE t TO app_ro;\nGRANT INSERT, UPDATE ON ALL TABLES IN SCHEMA app TO app_ro;");
        assert!(e.refers.is_empty(), "object grants leaked: {:?}", e.refers);
    }

    #[test]
    fn multiple_memberships_are_all_captured() {
        let e = parse("grant a to c;\ngrant b to c;");
        assert_eq!(e.refers, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn duplicate_grants_are_deduplicated() {
        let e = parse("grant a to c;\ngrant a to c;");
        assert_eq!(e.refers, vec!["a".to_string()]);
    }

    #[test]
    fn with_admin_option_is_still_a_membership() {
        let e = parse("grant a to c with admin option;");
        assert_eq!(e.refers, vec!["a".to_string()]);
    }

    /// A membership is a hard reference — the resolver must not treat it as a
    /// soft one the way it treats a body's function calls.
    #[test]
    fn memberships_are_hard_references() {
        let e = parse("grant a to c;");
        assert_eq!(e.references[0].ref_type, None);
    }

    /// Role is the one native type whose search path stays empty: a role name
    /// has no schema, so nothing is qualified against it, and the incumbent
    /// never reached the search-path extraction. Setting it would fail parity.
    #[test]
    fn search_paths_stay_empty() {
        let e = parse("grant a to c;");
        assert!(e.search_paths.is_empty(), "got {:?}", e.search_paths);
    }

    /// Role names are not schema-qualified.
    #[test]
    fn role_names_are_not_schema_qualified() {
        let e = parse("grant a to c;");
        assert_eq!(e.refers, vec!["a".to_string()], "must not become public.a");
    }

    #[test]
    fn a_role_file_with_no_grants_has_no_refers() {
        let e = parse("DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'solo') THEN\n    CREATE ROLE \"solo\";\n  END IF;\nEND $$;\n");
        assert!(e.refers.is_empty());
        assert!(e.errors.is_empty(), "a role with no grants is valid, got {:?}", e.errors);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("grant to to to;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }
}
```

- [ ] **Step 2: Register and confirm they fail**

In `pg/mod.rs` add `pub(crate) mod roles;` beside the other module declarations.

Run: `cargo test -p dbd-core --lib parser::pg::roles > /tmp/t.log 2>&1; echo "exit: $?"; tail -10 /tmp/t.log`
Expected: non-zero — `cannot find function parse_role in this scope`.

- [ ] **Step 3: Implement**

Prepend to `crates/dbd-core/src/parser/pg/roles.rs`:

```rust
//! Role DDL, parsed with libpg_query.
//!
//! Role files are idempotent-wrapped (`DO $$ … CREATE ROLE … END $$;`), which
//! sqlparser cannot read at all — hence the regex scanner this replaces. The
//! wrapper is a `DoStmt` here and simply ignored.
//!
//! The regex had to tell a role grant (`GRANT parent TO member`) from an object
//! grant (`GRANT SELECT ON TABLE t TO member`) by requiring `TO` to follow the
//! identifier immediately. Postgres parses them as different statement types, so
//! that exclusion is structural here: only `GrantRoleStmt` is a membership.

use crate::entity::{Entity, Reference};
use crate::error::Result;

/// Parse a role DDL file, recording its memberships as references.
pub(crate) fn parse_role(mut entity: Entity, sql: &str) -> Result<Entity> {
    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    // Deliberately NOT setting `search_paths`: a role name carries no schema, so
    // nothing is qualified against it, and the sqlparser path never reached the
    // search-path extraction either. Populating it here would be drift.
    let mut names: Vec<String> = Vec::new();
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::GrantRoleStmt(grant)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        for role in &grant.granted_roles {
            let Some(pg_query::NodeEnum::AccessPriv(priv_)) = role.node.as_ref() else {
                continue;
            };
            if priv_.priv_name.is_empty() || names.contains(&priv_.priv_name) {
                continue;
            }
            names.push(priv_.priv_name.clone());
        }
    }

    entity.references = names
        .iter()
        .map(|name| Reference {
            name: name.clone(),
            // A membership is a hard dependency, unlike a body's function calls.
            ref_type: None,
        })
        .collect();
    entity.refers = names;
    Ok(entity)
}
```

Note the order is source order (grants appear in file order), NOT sorted — `granted_roles` is a `Vec` on the statement, not a `HashSet`-derived accessor, so it is already deterministic. Confirm that: if you find ordering varies across runs, say so and sort, citing the precedent in `common::extract_view_refs_via_pg_query`.

Run: `cargo test -p dbd-core --lib parser::pg::roles > /tmp/t.log 2>&1; echo "exit: $?"; tail -12 /tmp/t.log`
Expected: exit 0, 11 tests pass.

- [ ] **Step 4: Delete the regex scanner and the early return**

In `crates/dbd-core/src/parser/mod.rs`:

- Delete the Role early-return block near the top of `parse_with_sqlparser`:
  ```rust
  if entity.entity_type == EntityType::Role {
      extract_role_memberships(sql, &mut entity);
      return Ok(entity);
  }
  ```
- Delete the `extract_role_memberships` function entirely, along with its doc comment.
- Remove any now-unused imports (`Reference` may still be used elsewhere — check).
- Check whether the `regex` crate is still used anywhere in `parser/`. `preprocess_sql` has three regex workarounds, so it probably is; report what remains.

**Careful:** `SqlparserDdl` must still handle Role sanely for a project that sets `source.parser: sqlparser`. After deleting the early return, Role falls through to sqlparser, which cannot parse `DO $$ … $$` — but the libpg_query validation fallback in the error arm should then accept it. Verify what a Role entity looks like under `ParserChoice::Sqlparser` after your change, and report it. If it now carries a parse error, that is a regression for the escape hatch and you should say so rather than proceeding.

- [ ] **Step 5: Add the corpus**

Role files are not schema-scoped — the path is `ddl/role/<name>.ddl`, with no schema directory.

`tests/fixtures/parser_corpus/ddl/role/app_ro.ddl`:
```sql
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_ro') THEN
    CREATE ROLE "app_ro";
  END IF;
END $$;
GRANT "app_admin" TO "app_ro";
```

`tests/fixtures/parser_corpus/ddl/role/bare_grant.ddl`:
```sql
do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'bare_grant') then
    create role bare_grant;
  end if;
end $$;
grant parent to bare_grant;
```

`tests/fixtures/parser_corpus/ddl/role/object_grants.ddl` — the case the regex had to exclude by lookahead:
```sql
do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'object_grants') then
    create role object_grants;
  end if;
end $$;
grant membership to object_grants;
grant select on table t to object_grants;
grant insert, update on all tables in schema app to object_grants;
```

`tests/fixtures/parser_corpus/ddl/role/no_grants.ddl`:
```sql
do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'no_grants') then
    create role no_grants;
  end if;
end $$;
```

- [ ] **Step 6: Switch Role over**

In `pg/mod.rs`, add `EntityType::Role` to `COVERED` and to `native()`:

```rust
            EntityType::Role => Some(roles::parse_role),
```

- [ ] **Step 7: Run the gate**

Run: `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"; tail -40 /tmp/p.log`

**If it FAILS, do NOT adjust the test, the fixtures, or the gate.** Report the exact diff. Three gate failures in this migration have each revealed a real bug.

Note: after Step 4 the incumbent's Role handling changed, so the gate is now comparing your parser against sqlparser-plus-fallback rather than against the regex. If they disagree, that is meaningful — report it.

Then run it three times in a row.

- [ ] **Step 8: Full verification**

- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

The existing role tests in `parser/mod.rs` (`role_membership_round_trip`, `role_with_no_grants_has_empty_refers`, `role_bare_identifier_grant_parsed`) must still pass — they now exercise the new path through `parse_entity`. If any fails, report the diff rather than editing the test.

- [ ] **Step 9: Verify live — role ordering and the emit round-trip**

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-role-check
rm -rf $R; mkdir -p $R/ddl/role $R/ddl/table/rl
cat > $R/design.yaml <<'YAML'
project:
  name: rl
  version: 1
source:
  dialect: postgresql
schemas:
  - rl
YAML
cat > $R/ddl/role/rl_admin.ddl <<'DDL'
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'rl_admin') THEN
    CREATE ROLE "rl_admin";
  END IF;
END $$;
DDL
cat > $R/ddl/role/rl_ro.ddl <<'DDL'
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'rl_ro') THEN
    CREATE ROLE "rl_ro";
  END IF;
END $$;
GRANT "rl_admin" TO "rl_ro";
DDL
cat > $R/ddl/table/rl/t.ddl <<'DDL'
set search_path to rl;
create table if not exists t (id int primary key);
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_rl' -c 'CREATE DATABASE dbd_rl'
cd $R
$DBD graph -s . | python3 -c "import json,sys; g=json.load(sys.stdin); print('edges :',g['edges']); print('layers:',g['layers'])"
$DBD apply -d postgresql://Jerry@localhost/dbd_rl -s .
psql -qtA -d dbd_rl -c "select r.rolname||' member_of '||coalesce(g.rolname,'-') from pg_roles r left join pg_auth_members m on m.member=r.oid left join pg_roles g on g.oid=m.roleid where r.rolname like 'rl_%' order by 1"
$DBD reconcile -d postgresql://Jerry@localhost/dbd_rl -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_rl -s . | tail -1
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_rl'
psql -q -d postgres -c 'DROP ROLE IF EXISTS rl_ro' -c 'DROP ROLE IF EXISTS rl_admin'
```

**Required:** the graph shows `rl_ro -> rl_admin`, with `rl_admin` in an earlier layer; apply succeeds; the catalog shows `rl_ro member_of rl_admin`; reconcile reports `0 created, 0 altered`; diff in sync.

**Note:** roles are cluster-wide, not per-database — the cleanup drops them explicitly. Do not skip that.

Report the ACTUAL output.

- [ ] **Step 10: Commit**

```bash
git add crates/dbd-core/src/parser/ tests/fixtures/parser_corpus/ddl/role
git commit -F - <<'MSG'
feat(parser): parse roles with libpg_query, deleting the last regex scanner

Fifth entity type to go native, and a strict improvement rather than a
port. Role files are idempotent-wrapped in DO $$ … $$, which sqlparser
cannot read, so parse_entity special-cased Role with an early return that
scanned the raw text with a regex.

That regex had to tell a role grant (GRANT parent TO member) from an
object grant (GRANT SELECT ON TABLE t TO member) by requiring TO to follow
the identifier immediately. Postgres parses those as different statement
types, so the exclusion is now structural: only GrantRoleStmt is a
membership, and GrantStmt is ignored by construction.

Role keeps an empty search_paths, unlike every other native type — a role
name carries no schema, so nothing is qualified against it.
MSG
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -p dbd-core --test parser_parity` exits 0 three consecutive times
- [ ] `extract_role_memberships` no longer exists anywhere
- [ ] Live: `rl_ro -> rl_admin` edge, catalog membership present, roles dropped afterwards
- [ ] `MaterializedView` still NOT in `COVERED`

## What this leaves

After this, the only regexes remaining in the parser are `preprocess_sql`'s three sqlparser workarounds (COMMENT ON object types, `PROCEDURE`→`FUNCTION`, matview `WITH DATA`). Those retire when the sqlparser path itself does — i.e. after Table (step 5) and MaterializedView (step 2b), which are the last two types.
