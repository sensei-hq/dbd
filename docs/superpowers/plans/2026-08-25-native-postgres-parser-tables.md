# Tables on libpg_query — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `EntityType::Table` onto libpg_query — the last type — behind the differential parity gate.

**Architecture:** Task 1 teaches `canonical_type` to strip `pg_catalog.`, so the two parsers' type spellings converge. Task 2 adds `pg/tables.rs` and switches over. The order is load-bearing.

**Tech Stack:** Rust 2024, `pg_query` 6, `sqlparser` 0.62.

**Spec:** `docs/superpowers/specs/2026-08-25-native-postgres-parser-tables-design.md`

---

## A note on this plan's shape

Every prior plan in this migration gave complete code for every step. **This one cannot, honestly.** Task 2 replaces `parser/tables.rs` — 752 lines of field-by-field sqlparser extraction. Transcribing an equivalent here would produce a worse result than having the implementer read the incumbent and mirror its semantics, because the incumbent *is* the specification: the parity gate compares against it exactly.

So Task 2 gives the structure, the measured facts, the traps, and the verification — and directs the implementer to `parser/tables.rs` for field-level behaviour. Task 1 is small and fully specified.

If Task 2 proves too large for one pass, **stop and report** rather than delivering a partially-correct table parser. A half-right one is worse than none: see the `--prune` risk below.

---

## Task 1: Teach `canonical_type` about `pg_catalog.`

**Files:**
- Modify: `crates/dbd-core/src/reconcile.rs` — `canonical_type` (around line 265)

### Why this is first, and separate

`canonical_type` maps type aliases so a parsed spelling and an introspected one compare equal. It strips a leading `public.` but not `pg_catalog.`.

Postgres rewrites SQL-standard type names at parse time, so libpg_query reports `pg_catalog.int4` where sqlparser reports `INT`. Measured over 18 common types, 9 differ this way. Because `pg_catalog.int4` contains a `.`, it falls through `canonical_type`'s alias match unmatched and stays as-is.

Measured: the two parsers' spellings converge on **3 of 12** cases today, and **12 of 12** with the prefix stripped.

Landing this after Task 2 would make every table in every project report spurious column-type drift. It is also correct independently — `pg_catalog.int4` and `integer` name the same type whoever produced the string.

- [ ] **Step 1: Write the failing tests**

Add to `reconcile.rs`'s `mod tests`:

```rust
    // ── pg_catalog-qualified type names ─────────────────────────────────────

    /// Postgres rewrites SQL-standard type names at parse time, so libpg_query
    /// reports `pg_catalog.int4` where sqlparser reports `INT`. Both name the
    /// same type; the alias table below already knows `int4`, it just never saw
    /// it because the qualified form does not reach the match.
    #[test]
    fn pg_catalog_qualified_types_normalize_like_their_bare_form() {
        let e = std::collections::HashMap::new();
        for (qualified, bare) in [
            ("pg_catalog.int4", "int"),
            ("pg_catalog.int8", "bigint"),
            ("pg_catalog.int2", "smallint"),
            ("pg_catalog.bool", "boolean"),
            ("pg_catalog.varchar(30)", "varchar(30)"),
            ("pg_catalog.bpchar(2)", "char(2)"),
            ("pg_catalog.numeric(10,2)", "numeric(10,2)"),
            ("pg_catalog.timestamptz", "timestamp with time zone"),
        ] {
            assert_eq!(
                canonical_type(qualified, &e),
                canonical_type(bare, &e),
                "{qualified} and {bare} must canonicalize alike"
            );
        }
    }

    /// Only the two system schemas are stripped. A user type keeps its schema —
    /// `app.status_t` and `other.status_t` are different types.
    #[test]
    fn a_user_schema_qualification_is_preserved() {
        let e = std::collections::HashMap::new();
        assert_eq!(canonical_type("app.status_t", &e), "app.status_t");
        assert_ne!(canonical_type("app.status_t", &e), canonical_type("other.status_t", &e));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dbd-core --lib reconcile::tests::pg_catalog > /tmp/t.log 2>&1; echo "exit: $?"; tail -12 /tmp/t.log`
Expected: non-zero, showing `pg_catalog.int4` != `integer`.

- [ ] **Step 3: Implement**

In `canonical_type`, extend the existing `public.` strip:

```rust
    // `public.` and `pg_catalog.` are the implicit search path — a type named
    // through either is the same type as the bare form. Postgres rewrites
    // SQL-standard names into pg_catalog-qualified internals at parse time
    // (`int` → `pg_catalog.int4`), so without this the alias table below never
    // sees them and every such column reads as drifted.
    for prefix in ["public.", "pg_catalog."] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
```

- [ ] **Step 4: Verify**

- `cargo test -p dbd-core --lib reconcile > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

Existing reconcile tests exercise `canonical_type` heavily — if one changes behaviour, report the diff rather than editing it.

- [ ] **Step 5: Verify live — this must not change current behaviour**

Task 1 alone should be invisible: nothing yet produces `pg_catalog.`-qualified types.

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-canon
rm -rf $R; mkdir -p $R/ddl/table/cn
cat > $R/design.yaml <<'YAML'
project:
  name: cn
  version: 1
source:
  dialect: postgresql
schemas:
  - cn
YAML
cat > $R/ddl/table/cn/t.ddl <<'DDL'
set search_path to cn;
create table if not exists t (
  id   int primary key,
  code varchar(30) not null,
  flag boolean not null default false,
  ts   timestamp with time zone,
  amt  numeric(10,2)
);
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_canon' -c 'CREATE DATABASE dbd_canon'
cd $R
$DBD apply -d postgresql://Jerry@localhost/dbd_canon -s .
$DBD reconcile -d postgresql://Jerry@localhost/dbd_canon -s . | tail -1
$DBD reconcile -d postgresql://Jerry@localhost/dbd_canon -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_canon -s . | tail -1
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_canon'
```

**Required:** both reconciles `0 created, 0 altered`, diff in sync. Any `ALTER COLUMN … TYPE` here means the change altered live behaviour — report it, do not proceed to Task 2.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/reconcile.rs
git commit -F - <<'MSG'
fix(reconcile): canonicalize pg_catalog-qualified type names

canonical_type strips a leading `public.` so a parsed and an introspected
spelling of the same type compare equal, but never stripped `pg_catalog.`
— and because the qualified form contains a dot, it fell through the alias
match untouched.

Postgres rewrites SQL-standard type names into pg_catalog-qualified
internals at parse time, so `int` becomes `pg_catalog.int4`. Measured over
12 spellings, a parser reporting the rewritten form agreed with one
reporting the authored form in 3 cases; with the prefix stripped, 12.

Inert today — nothing produces the qualified form yet — and a prerequisite
for the native table parser, which will.
MSG
```

---

## Task 2: Native table parsing and switchover

**Files:**
- Create: `crates/dbd-core/src/parser/pg/tables.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`
- Modify: `crates/dbd-core/src/parser/pg/matviews.rs` — replace its minimal index walker
- Create: `tests/fixtures/parser_corpus/ddl/table/app/*.ddl`

### Read this first

**`crates/dbd-core/src/parser/tables.rs` is the specification.** The parity gate compares your output against it field for field, so its semantics — including ones that look odd — are the target. Read it fully before writing anything.

One that looks odd and is deliberate: a composite `PRIMARY KEY (a, b)` produces **both** a table-level `TableConstraint::PrimaryKey` **and** `is_pk = true` on each member column. `lift_pk_unique_keep_others` and `pk_unique_col_sets` in `reconcile.rs` depend on that exact shape, and `pk_unique_col_sets` documents a real bug caused by reading those flags naively. **Reproduce it. Do not rationalise it here.**

### The safety rule that overrides everything

`reconcile::raw_snapshot_from_entities` (`reconcile.rs:97`) filters on
`e.table_def.is_some()`. A table absent from the desired snapshot reads as an
**orphan**, and `--prune` drops it.

So: **if any part of a table cannot be extracted, push an error onto the entity**
rather than returning a partial `table_def` or `None`. `ensure_fully_parsed` then
refuses the whole run, which is loud and safe. This is the one type where
erroring beats degrading — every other native parser degrades gracefully; this
one must not.

### Verified AST facts (measured — do not re-derive)

- `CREATE TABLE` → `NodeEnum::CreateStmt`, with `relation` (schema + name),
  `if_not_exists`, and `table_elts`.
- `table_elts` entries are either `NodeEnum::ColumnDef` or `NodeEnum::Constraint`.
- A `ColumnDef` carries `colname`, `type_name` (with `names`, `typmods`,
  `array_bounds`), and `constraints` — each a `Constraint` whose `contype()` is
  one of `ConstrPrimary`, `ConstrNotnull`, `ConstrDefault`, `ConstrForeign`,
  `ConstrUnique`, `ConstrIdentity`, `ConstrCheck`.
- A table-level `Constraint` carries `conname` and `keys`.
- `COMMENT ON COLUMN` and `CREATE INDEX` arrive as sibling `CommentStmt` /
  `IndexStmt` statements, not inside `CreateStmt`.
- Type names come back pg_catalog-qualified for SQL-standard types
  (`int` → `pg_catalog.int4`). **Task 1 handles that** — do not strip it in the
  parser; let `canonical_type` do its job, because the raw `data_type` string is
  what the gate compares.

### Suggested sequencing within the task

Land as one commit (an unwired `parse_table` is dead code), but build in this
order so each piece is testable:

1. Columns: name, type, nullability, default, identity.
2. Inline column constraints: PK, unique, FK.
3. Table-level constraints: PK, unique, FK, check — named and unnamed.
4. `COMMENT ON COLUMN` → `ColumnDef.comment`.
5. Indexes — the fullest surface. Replace the detect-and-skip walker in
   `pg/matviews.rs` with this full version and drop its skip warning, since a
   table's indexes are not optional decoration.

- [ ] **Step 1: Build the corpus first**

Unusually, write the fixtures **before** the parser. They are the specification
made concrete, and the gate runs against them. Under
`tests/fixtures/parser_corpus/ddl/table/app/`, cover at least:

- `simple.ddl` — a few columns, inline PK, `NOT NULL`, defaults
- `composite_key.ddl` — `primary key (a, b)`, exercising the dual-shape rule
- `named_constraints.ddl` — named PK/unique/check/FK
- `inline_fk.ddl` — column-level `references` with `on delete cascade`
- `identity.ddl` — `generated always as identity` and `generated by default`
- `types.ddl` — `int`, `varchar(30)`, `char(2)`, `numeric(10,2)`, `boolean`,
  `timestamp with time zone`, `text[]`, `uuid`, `jsonb`, a user enum
- `indexes.ddl` — unique, partial (`where`), expression key, `include`,
  an explicit access method, `nulls not distinct`, `with (fillfactor = 70)`
- `comments.ddl` — `comment on column`
- `quoted.ddl` — quoted mixed-case identifiers

Run the gate now — it must still pass, since `Table` is not yet in `COVERED` and
these files are simply not swept.

- [ ] **Step 2: Write the parser, TDD per sub-area**

For each of the five sub-areas, write failing unit tests in `pg/tables.rs` first,
then implement. Mirror `parser/tables.rs` for semantics.

- [ ] **Step 3: Switch over and run the gate**

Add `EntityType::Table` to `COVERED` and `native()`. **Do NOT add it to
`NO_SECOND_IMPLEMENTATION`** — unlike Role and MaterializedView, sqlparser
produces a complete `table_def`, so this type both can and must be gated.

Run: `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"; tail -60 /tmp/p.log`

**Expect this to fail the first time, probably several times.** That is the gate
doing its job on the largest contract in the codebase. For each failure, report
the diff and fix the **parser**, never the fixture or the gate. Three of this
migration's real bugs surfaced exactly this way.

Then three consecutive clean runs.

- [ ] **Step 4: Full verification**

- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 5: Verify live — including the prune hazard**

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-tbl
rm -rf $R; mkdir -p $R/ddl/table/tb $R/ddl/enum/tb
cat > $R/design.yaml <<'YAML'
project:
  name: tb
  version: 1
source:
  dialect: postgresql
schemas:
  - tb
YAML
cat > $R/ddl/enum/tb/status_t.ddl <<'DDL'
set search_path to tb;
create type status_t as enum ('active','archived');
DDL
cat > $R/ddl/table/tb/parent.ddl <<'DDL'
set search_path to tb;
create table if not exists parent (id uuid primary key);
DDL
cat > $R/ddl/table/tb/child.ddl <<'DDL'
set search_path to tb;
create table if not exists child (
  id     uuid primary key default gen_random_uuid(),
  pid    uuid not null references parent(id) on delete cascade,
  code   varchar(30) not null,
  status status_t not null default 'active',
  qty    int not null default 0 check (qty >= 0),
  n      int generated always as identity,
  constraint child_code_uk unique (code, pid)
);
comment on column child.code is 'the code';
create unique index child_code_idx on child(code) where qty > 0;
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_tbl' -c 'CREATE DATABASE dbd_tbl'
cd $R
$DBD apply -d postgresql://Jerry@localhost/dbd_tbl -s .
$DBD reconcile -d postgresql://Jerry@localhost/dbd_tbl -s . | tail -1
$DBD reconcile -d postgresql://Jerry@localhost/dbd_tbl -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_tbl -s . | tail -1
echo "--- PRUNE SAFETY: --prune must NOT drop a table it parsed fine ---"
$DBD reconcile --prune -d postgresql://Jerry@localhost/dbd_tbl -s . | tail -3
psql -qtA -d dbd_tbl -c "select tablename from pg_tables where schemaname='tb' order by 1"
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_tbl'
```

**Required:** both reconciles `0 created, 0 altered`; diff in sync; and after
`--prune`, **both `child` and `parent` still exist**. A missing table here is the
data-loss scenario the spec warns about — report it immediately.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/parser/pg/ tests/fixtures/parser_corpus/ddl/table
git commit -F - <<'MSG'
feat(parser): parse tables with libpg_query

The last type. Columns, types, nullability, defaults, identity, inline and
table-level constraints, column comments and indexes now come from
Postgres's own grammar.

A table is the one type where a parsing failure can lose data:
raw_snapshot_from_entities filters on table_def.is_some(), so a table dbd
cannot read is absent from the desired snapshot, reads as an orphan, and is
dropped by --prune. Anything unextractable therefore errors the entity so
ensure_fully_parsed refuses the run, rather than degrading to a partial
table_def the way the other native parsers may.

Kept out of NO_SECOND_IMPLEMENTATION deliberately: sqlparser produces a
complete table_def, so this is the one remaining type the differential gate
can check — and the one where it is worth the most.
MSG
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -p dbd-core --test parser_parity` exits 0 three consecutive times
- [ ] `Table` is in `COVERED` and **not** in `NO_SECOND_IMPLEMENTATION`
- [ ] Live: `--prune` leaves both tables intact
- [ ] Two consecutive reconciles report `0 created, 0 altered`

## Explicitly NOT in this plan

Retiring the sqlparser scaffolding — `preprocess_sql`'s three regex workarounds,
the tier-3 text scan, `SqlparserDdl` itself, and the parity gate — is a separate
change, made **after** this one has soaked. Doing it in the same commit would
remove the very thing that verifies this one.
