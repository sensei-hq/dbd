# Materialized Views on libpg_query — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `EntityType::MaterializedView` onto libpg_query, storing the **verbatim source body** in `writes[0]`, without every existing matview reporting spurious drift on upgrade.

**Architecture:** Two tasks, in order. First the drift sentinel gains a `v2:` version prefix and a silent re-stamp path for `v1` stamps — landed **before** anything changes the hash. Then `pg/matviews.rs` extracts the body verbatim via `pg_query::scan()` token offsets and takes over dispatch.

**Tech Stack:** Rust 2024, `pg_query` 6, `sha2`.

**Spec:** `docs/superpowers/specs/2026-08-25-native-postgres-parser-matviews-design.md`

**Prior increments:** enum, view, function/procedure, role — see their plans under `docs/superpowers/plans/`.

---

## Order matters, and this is the reason

`reconcile::matview_hash` hashes `normalize_matview_body(entity)`, which reads `writes[0]`. Task 2 changes what `writes[0]` holds, so it changes the hash of **every existing matview**. If Task 1 has not landed first, every dbd-managed matview in every project reports drift once, indistinguishable from real drift, with a message telling users to `DROP … CASCADE`.

Task 1 is independently useful and has no dead code — it modifies existing wired-up logic. Task 2 is bundled (parser + switchover) for the usual reason: an unwired `parse_matview` is dead code under `clippy --all-targets -- -D warnings`.

---

## Task 1: Version the drift sentinel

**Files:**
- Modify: `crates/dbd-core/src/reconcile.rs` — `matview_hash_comment_sql`, `parse_dbd_hash`, `MatviewAction`, `decide_matview_action`
- Modify: `crates/dbd-core/src/design/reconcile.rs` — handle the new action in the write pass
- Modify: `crates/dbd-core/src/design/apply.rs:178` — stamp the new format

### The trap you must not walk into

`parse_dbd_hash` currently reads:

```rust
let rest = comment?.split("dbd:hash=").nth(1)?;
let hash: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
```

`take_while(is_ascii_alphanumeric)` **stops at `:`**. So a naive `dbd:hash=v2:<hash>` stamp parses back as the string `"v2"` — silently wrong, and it would compare unequal to every real hash, turning every matview into a permanent false drift. The parser must be updated in the same change as the format.

- [ ] **Step 1: Write the failing tests**

Add to `reconcile.rs`'s existing `mod tests`:

```rust
    // ── Sentinel versioning ─────────────────────────────────────────────────

    #[test]
    fn v2_stamp_round_trips() {
        let sql = matview_hash_comment_sql("a.m", "deadbeefcafe0001");
        assert!(sql.contains("dbd:hash=v2:deadbeefcafe0001"), "got: {sql}");
        let payload = sql.split_once("IS '").unwrap().1.trim_end_matches("';");
        assert_eq!(
            parse_dbd_hash(Some(payload)),
            Some(Sentinel::V2("deadbeefcafe0001".to_string()))
        );
    }

    /// A stamp written by an older dbd has no version prefix. It must be
    /// recognised as v1, not misparsed — `take_while(is_ascii_alphanumeric)`
    /// would stop at the `:` of a v2 stamp and yield "v2".
    #[test]
    fn unversioned_stamp_parses_as_v1() {
        assert_eq!(
            parse_dbd_hash(Some("dbd:hash=deadbeefcafe0001")),
            Some(Sentinel::V1("deadbeefcafe0001".to_string()))
        );
    }

    #[test]
    fn absent_or_foreign_comment_has_no_sentinel() {
        assert_eq!(parse_dbd_hash(None), None);
        assert_eq!(parse_dbd_hash(Some("just a comment")), None);
    }

    /// A v1 stamp means "written by a dbd that hashed the old body contract",
    /// so its hash is not comparable — re-stamp silently rather than warn.
    #[test]
    fn a_v1_stamp_is_restamped_not_warned() {
        assert!(matches!(
            decide_matview_action(Some(Some(Sentinel::V1("anything".into()))), "want"),
            MatviewAction::Restamp
        ));
    }

    #[test]
    fn a_matching_v2_stamp_is_skipped() {
        assert!(matches!(
            decide_matview_action(Some(Some(Sentinel::V2("want".into()))), "want"),
            MatviewAction::Skip
        ));
    }

    #[test]
    fn a_differing_v2_stamp_is_real_drift() {
        assert!(matches!(
            decide_matview_action(Some(Some(Sentinel::V2("other".into()))), "want"),
            MatviewAction::Warn
        ));
    }

    #[test]
    fn an_unstamped_matview_still_warns() {
        assert!(matches!(decide_matview_action(Some(None), "want"), MatviewAction::Warn));
    }

    #[test]
    fn an_absent_matview_is_created() {
        assert!(matches!(decide_matview_action(None, "want"), MatviewAction::Create));
    }
```

Adapt names to the real ones — check `decide_matview_action`'s actual signature and `MatviewAction`'s variants before writing, and report any mismatch.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dbd-core --lib reconcile::tests > /tmp/t.log 2>&1; echo "exit: $?"; tail -8 /tmp/t.log`
Expected: non-zero — `Sentinel` does not exist.

- [ ] **Step 3: Implement**

Add the sentinel type near `parse_dbd_hash`:

```rust
/// A parsed `dbd:hash` stamp.
///
/// Versioned because the hash input is the matview's `writes[0]`, so any change
/// to what that field holds changes every stamp. Without a version, such a
/// change makes every existing matview report drift once — indistinguishable
/// from the real thing, and the warning tells users to `DROP … CASCADE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sentinel {
    /// Written by a dbd that hashed the pre-verbatim body contract. Its hash is
    /// not comparable with a current one, so it is re-stamped rather than read.
    V1(String),
    /// Current format.
    V2(String),
}
```

Rewrite `parse_dbd_hash` to return `Option<Sentinel>`. Take chars while alphanumeric **or `:`**, then split on the version prefix:

```rust
pub(crate) fn parse_dbd_hash(comment: Option<&str>) -> Option<Sentinel> {
    let rest = comment?.split("dbd:hash=").nth(1)?;
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == ':')
        .collect();
    match token.strip_prefix("v2:") {
        Some(hash) if !hash.is_empty() => Some(Sentinel::V2(hash.to_string())),
        // An unversioned token is a v1 stamp. Reject anything else that carries a
        // colon — an unknown future version must not be read as a v1 hash.
        _ if !token.is_empty() && !token.contains(':') => Some(Sentinel::V1(token)),
        _ => None,
    }
}
```

Update `matview_hash_comment_sql` to emit `dbd:hash=v2:{hash}`.

Add `Restamp` to `MatviewAction` and update `decide_matview_action`:

```rust
    match stored {
        None => MatviewAction::Create,
        Some(Some(Sentinel::V2(h))) if h == want_hash => MatviewAction::Skip,
        // Re-stamp rather than warn: a v1 hash covers a different body contract,
        // so a mismatch says nothing about whether the definition actually drifted.
        Some(Some(Sentinel::V1(_))) => MatviewAction::Restamp,
        Some(_) => MatviewAction::Warn,
    }
```

- [ ] **Step 4: Handle `Restamp` in the write pass**

In `crates/dbd-core/src/design/reconcile.rs`, `detect_reconcile_matviews` records decisions before the `if dry_run { return }` at roughly line 300; only writes run after it. Collect `Restamp` matviews the same way `mv_to_create` is collected, and in the write pass issue **only** the comment SQL (`matview_hash_comment_sql`) — never a `CREATE`, never a `DROP`.

Two requirements:

1. **It must not fire under `--dry-run`.** The existing structure gives this for free if you put the write after the return — verify it, do not assume.
2. **It must be visible in the summary**, not silent. A re-stamp is a write to a live object; surface a count. Check `ReconcileComplete`'s fields and follow the existing style.

- [ ] **Step 5: Update the apply-path stamp**

`crates/dbd-core/src/design/apply.rs:178` writes the sentinel when applying. It calls `matview_hash_comment_sql`, so the format change flows through — but confirm and say so.

- [ ] **Step 6: Verify**

- `cargo test -p dbd-core --lib reconcile > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 7: Verify live — the v1 upgrade path is the whole point**

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-mv1
rm -rf $R; mkdir -p $R/ddl/table/mv $R/ddl/materialized_view/mv
cat > $R/design.yaml <<'YAML'
project:
  name: mv
  version: 1
source:
  dialect: postgresql
schemas:
  - mv
YAML
cat > $R/ddl/table/mv/t.ddl <<'DDL'
set search_path to mv;
create table if not exists t (id int primary key, total int);
DDL
cat > $R/ddl/materialized_view/mv/m.ddl <<'DDL'
set search_path to mv;
create materialized view m as select id, total from t where total > 0 with data;
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_mv1' -c 'CREATE DATABASE dbd_mv1'
cd $R
$DBD apply -d postgresql://Jerry@localhost/dbd_mv1 -s .
echo "--- stamp written (expect v2:) ---"
psql -qtA -d dbd_mv1 -c "select obj_description('mv.m'::regclass)"
echo "--- forge a v1 stamp, as an older dbd would have left ---"
psql -qtA -d dbd_mv1 -c "comment on materialized view mv.m is 'dbd:hash=0123456789abcdef'"
echo "--- reconcile: must RE-STAMP silently, not warn ---"
$DBD reconcile -d postgresql://Jerry@localhost/dbd_mv1 -s .
psql -qtA -d dbd_mv1 -c "select obj_description('mv.m'::regclass)"
echo "--- second reconcile: now a no-op ---"
$DBD reconcile -d postgresql://Jerry@localhost/dbd_mv1 -s . | tail -2
echo "--- dry-run must NOT write ---"
psql -qtA -d dbd_mv1 -c "comment on materialized view mv.m is 'dbd:hash=0123456789abcdef'"
$DBD reconcile --dry-run -d postgresql://Jerry@localhost/dbd_mv1 -s . | tail -2
psql -qtA -d dbd_mv1 -c "select obj_description('mv.m'::regclass)"
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_mv1'
```

**Required:** the first stamp is `dbd:hash=v2:…`; after forging a v1 stamp, reconcile re-stamps it to `v2:` **without** a drift warning; the next reconcile is a no-op; and after `--dry-run` the forged v1 stamp is **still `dbd:hash=0123456789abcdef`** — unchanged, proving no write.

Report the ACTUAL output.

- [ ] **Step 8: Commit**

```bash
git add crates/dbd-core/src/reconcile.rs crates/dbd-core/src/design/
git commit -F - <<'MSG'
feat(reconcile): version the matview drift sentinel

matview_hash reads writes[0], so any change to what that field holds
changes the stamp on every existing matview — every one would report drift
once, indistinguishable from the real thing, with a message telling users
to DROP … CASCADE. That is the same class of spurious warning the hash's
own doc comment says SHA-256 was chosen to avoid.

Stamps are now `dbd:hash=v2:<hash>`. An unversioned stamp is recognised as
v1 — written against a different body contract, so its hash says nothing
about drift — and is re-stamped silently instead of warned. The next
contract change bumps to v3 with the same one-line treatment.

parse_dbd_hash had to change with the format: it took chars while
alphanumeric, which stops at the `:` of a v2 stamp and would have yielded
the literal "v2" as the hash.
MSG
```

---

## Task 2: Native matview parsing with a verbatim body

**Files:**
- Create: `crates/dbd-core/src/parser/pg/matviews.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`
- Modify: `crates/dbd-core/tests/parser_parity.rs` — add to `NO_SECOND_IMPLEMENTATION`
- Create: `tests/fixtures/parser_corpus/ddl/materialized_view/app/*.ddl`

### What the entity must carry

The sqlparser arm sets three things:

```rust
entity.writes = vec![extract_view_body(&statements)];   // the body
entity.references = extract_view_info(&statements, …);  // relations + function calls
entity.table_def = Some(tables::extract_table(…));      // trailing CREATE INDEX
```

References work exactly as for plain views — reuse `pg::views`' approach (`select_tables()` + `call_functions()`, functions tagged `REF_TYPE_FUNCTION`). Indexes come from trailing `CREATE INDEX` statements. The body is the novel part.

### Extracting the verbatim body

Verified facts (do not re-derive):

- `RawStmt.stmt_location`/`stmt_len` bound the **whole statement**. `stmt_len == 0` means "to end of input" — needed for a trailing statement.
- Inner node locations point at *contents*: `targetList[0].location` is the first column, not the `SELECT` keyword. Unusable for this.
- `pg_query::scan(sql)` returns `ScanToken { start, end, token, keyword_kind }` with byte offsets. It tokenizes correctly, so an `as` inside a string literal, comment or dollar-quoting is never mistaken for the keyword.
- **`ScanToken.token` is a numeric protobuf code** (`as` = 295, `select` = 651, `with` = 747 as measured), NOT a named variant in `Debug`. Match on the token's source text (`sql[start..end]`, lowercased), not the integers — they are an internal detail that could shift on a `pg_query` bump.

Algorithm:

1. Find the `CreateTableAsStmt` and its `RawStmt` range.
2. Scope the token list to that byte range.
3. Find the first `as` keyword token in range → body starts after it.
4. Find the last `with` keyword token in range that follows the `as` → body ends there (this is `WITH DATA` / `WITH NO DATA`). If absent, body runs to the end of the statement.
5. Slice the **original** text, trim, strip a trailing `;`.

- [ ] **Step 1: Write the failing tests**

Create `crates/dbd-core/src/parser/pg/matviews.rs` with ONLY this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        parse_matview(Entity::new(EntityType::MaterializedView, "app.m"), sql).unwrap()
    }

    fn body(sql: &str) -> String {
        parse(sql).writes.first().cloned().unwrap_or_default()
    }

    /// The whole point of this change: the author's SQL survives, rather than a
    /// parser's re-rendering of it.
    #[test]
    fn the_body_is_verbatim_not_re_rendered() {
        let e = body("set search_path to app;\ncreate materialized view m as\n  select a,\n         b\n  from t\nwith data;");
        assert_eq!(e, "select a,\n         b\n  from t");
    }

    #[test]
    fn with_no_data_is_not_part_of_the_body() {
        assert_eq!(body("create materialized view m as select a from t with no data;"), "select a from t");
    }

    #[test]
    fn a_missing_with_clause_still_yields_the_body() {
        assert_eq!(body("create materialized view m as select a from t;"), "select a from t");
    }

    /// The tokenizer must not mistake an `as` inside a string for the keyword.
    #[test]
    fn an_as_inside_a_string_literal_is_not_the_boundary() {
        let b = body("create materialized view m as select 'x as y' as lbl from t with data;");
        assert_eq!(b, "select 'x as y' as lbl from t");
    }

    #[test]
    fn a_comment_before_the_body_does_not_break_extraction() {
        let b = body("create materialized view m as -- a note\n  select a from t\nwith data;");
        assert!(b.contains("select a from t"), "got {b:?}");
    }

    #[test]
    fn a_trailing_create_index_is_not_part_of_the_body() {
        let b = body("create materialized view m as select a from t with data;\ncreate index i on m(a);");
        assert_eq!(b, "select a from t");
    }

    #[test]
    fn a_trailing_index_lands_in_table_def() {
        let e = parse("create materialized view m as select a from t with data;\ncreate unique index i on m(a);");
        let ix = e.table_def.as_ref().expect("table_def").indexes.clone();
        assert_eq!(ix.len(), 1, "got {ix:?}");
        assert!(ix[0].unique);
    }

    #[test]
    fn relations_and_function_calls_become_references() {
        let e = parse("set search_path to app;\ncreate materialized view m as select app.myfn(a) from t with data;");
        assert!(e.refers.contains(&"app.t".to_string()), "got {:?}", e.refers);
        assert!(e.refers.contains(&"app.myfn".to_string()), "got {:?}", e.refers);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate materialized view m as select a from t with data;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create materialized view m as select a from t with data;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create materialized view m as select * from ;");
        assert!(!e.errors.is_empty());
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    #[test]
    fn a_file_declaring_no_matview_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty());
    }
}
```

The exact expected string in `the_body_is_verbatim_not_re_rendered` depends on your trimming. Run it, see what you get, and if the difference is only leading/trailing whitespace, adjust the **expectation** to the natural result — but the body must retain its internal newlines and indentation. If it does not, the extraction is wrong, not the test.

- [ ] **Step 2: Register, confirm failure, implement**

Add `pub(crate) mod matviews;` to `pg/mod.rs`. Confirm the tests fail with `cannot find function parse_matview`. Then implement per the algorithm above, reusing `pg::views`' reference logic (extract a shared helper if that is cleaner than duplicating — say which you did).

For the trailing `CREATE INDEX`, the sqlparser path calls `tables::extract_table`. That is sqlparser-based, so you need the libpg_query equivalent: walk `IndexStmt` nodes. Check whether `pg::` already has index extraction from the Table work — it does not (Table is not native yet), so this is new. Keep it minimal: the fields `emit_index_sql` needs, matching what `extract_table` produces for a matview's indexes. **If this turns out to be substantial, STOP and report** — it may deserve its own task rather than being smuggled in here.

- [ ] **Step 3: Exclude from the parity gate**

Add `EntityType::MaterializedView` to `NO_SECOND_IMPLEMENTATION` in `crates/dbd-core/tests/parser_parity.rs`, extending the doc comment:

```rust
/// `MaterializedView` is excluded for a different reason than `Role`: here the
/// two implementations are *intended* to disagree. The incumbent stores
/// sqlparser's re-rendering of the body in `writes[0]`; the native parser stores
/// the verbatim source. A gate asserting they match would assert the change did
/// not happen.
```

- [ ] **Step 4: Corpus and switchover**

Add fixtures under `tests/fixtures/parser_corpus/ddl/materialized_view/app/` covering: a plain matview, `WITH NO DATA`, a trailing `CREATE INDEX`, a multi-line body with indentation, and one whose body contains the word `as` inside a string literal.

Add `EntityType::MaterializedView` to `COVERED` and `native()`.

- [ ] **Step 5: Verify**

- `cargo test -p dbd-core --lib parser::pg::matviews > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"` → 0, three consecutive runs
- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 6: Verify live — round-trip and convergence**

Reuse the Task 1 live script, then additionally:

```bash
# after apply, confirm emit round-trips the AUTHOR's formatting
$DBD dbml -s . 2>/dev/null | head -5 || true
# and that reconcile converges rather than re-detecting drift
$DBD reconcile -d postgresql://Jerry@localhost/dbd_mv2 -s . | tail -1
$DBD reconcile -d postgresql://Jerry@localhost/dbd_mv2 -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_mv2 -s . | tail -1
```

**Required:** both reconciles report `0 created, 0 altered`; diff in sync. Since Task 1 landed first, there must be **no** drift warning for the matview.

- [ ] **Step 7: Commit**

```bash
git add crates/dbd-core/src/parser/pg/ crates/dbd-core/tests/parser_parity.rs tests/fixtures/parser_corpus/ddl/materialized_view
git commit -F - <<'MSG'
feat(parser): parse materialized views with libpg_query, body verbatim

Last type before Table. writes[0] now holds the author's SQL rather than
sqlparser's re-rendering of it, so emit_matview round-trips what was
written instead of what a parser made of it. The two renderers agreed on
only 10 of 14 realistic bodies raw — 12 of 14 under the normalization the
runtime applies — and storing a re-rendering at all was the defect.

The body comes from pg_query::scan() token offsets: statement offsets bound
the whole CREATE, and inner node locations point at contents rather than
the SELECT keyword, so neither can delimit it. Tokenizing means an `as`
inside a string, comment or dollar-quoting is never mistaken for the
boundary.

Excluded from the parity gate for the opposite reason to Role: there the
two paths are the same function, here they are intended to differ, and a
gate asserting they match would assert the change did not happen.
MSG
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] A forged v1 stamp is re-stamped to `v2:` with no drift warning
- [ ] `--dry-run` leaves a forged v1 stamp untouched
- [ ] A matview body retains its internal newlines and indentation
- [ ] Two consecutive reconciles report `0 created, 0 altered`
- [ ] `Table` is still NOT in `COVERED`

## What this leaves

Only `EntityType::Table` remains on sqlparser. When it goes native, `SqlparserDdl` has no callers left, `preprocess_sql`'s three regex workarounds retire with it, and the parity gate retires deliberately rather than silently self-comparing every type — see the parent spec's note on the gate not outliving the second implementation.
