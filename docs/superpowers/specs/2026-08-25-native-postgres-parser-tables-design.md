# Tables on libpg_query — design spec

Rollout step 5, the last type. After this, `SqlparserDdl` has no callers.

**Parent spec:** `2026-08-24-native-postgres-parser-design.md`

## Why this one is different

Every other type carried a small, well-bounded payload. A table carries the
largest contract in the codebase — `TableDef { columns, constraints, indexes,
comments }` — and it is the only type where getting it wrong risks **data loss**:

`reconcile::raw_snapshot_from_entities` (`reconcile.rs:97`) filters on
`e.table_def.is_some()`. A table dbd cannot structurally read is absent from the
desired snapshot, so the live table reads as an **orphan**, and `--prune` drops
it. Every other type fails loudly; this one can fail by deletion.

That asymmetry drives every decision below.

## The contract to reproduce

| field | notes |
| --- | --- |
| `ColumnDef.name`, `.data_type` | see the type-spelling section — this is the hard part |
| `.nullable`, `.default_value` | defaults compare against Postgres's round-tripped form, already normalized by `canonical_default` (`reconcile.rs:313`) |
| `.is_pk`, `.is_unique` | inline flags; the parser emits BOTH these and a table-level constraint for a composite key |
| `.identity` | `GENERATED { ALWAYS \| BY DEFAULT } AS IDENTITY` |
| `.comment` | from trailing `COMMENT ON COLUMN` statements |
| `.inline_fk` | column-level `REFERENCES` |
| `TableConstraint` | `PrimaryKey`, `Unique`, `ForeignKey`, `Check` — each optionally named |
| `IndexDef` | name, columns (order / nulls_first / opclass / is_expression), unique, access method, `predicate`, `include`, `nulls_not_distinct`, `with_options` |

All of it is present in libpg_query's AST. Measured on a realistic table, a
single `CreateStmt` yields column names, type names, and per-column constraint
kinds (`ConstrPrimary`, `ConstrNotnull`, `ConstrDefault`, `ConstrForeign`,
`ConstrUnique`, `ConstrIdentity`, `ConstrCheck`), plus named table-level
constraints with their key lists; `CommentStmt` and `IndexStmt` arrive as
sibling statements. Feasibility is not in question.

## The blocker: type spelling

Postgres normalizes SQL-standard type names into its internal ones at parse
time. sqlparser reports what the user typed. Measured over 18 common types,
**9 differ**:

| authored | sqlparser | libpg_query |
| --- | --- | --- |
| `int` / `integer` | `INT` / `INTEGER` | `pg_catalog.int4` |
| `bigint` | `BIGINT` | `pg_catalog.int8` |
| `varchar(30)` | `VARCHAR(30)` | `pg_catalog.varchar(30)` |
| `char(2)` | `CHAR(2)` | `pg_catalog.bpchar(2)` |
| `boolean` | `BOOLEAN` | `pg_catalog.bool` |
| `timestamp with time zone` | as typed | `pg_catalog.timestamptz` |
| `text`, `uuid`, `jsonb`, `date`, `bytea`, `text[]`, `app.status_t`, `serial` | — | **identical** |

The split is systematic: a type with a SQL-standard alias gets rewritten; one
without passes through.

dbd already has the machinery to absorb this. `reconcile::canonical_type`
(`reconcile.rs:265`) lowercases, splits `base(args)`, and maps aliases
(`int4`→`integer`, `bool`→`boolean`, `varchar`→`character varying`,
`bpchar`→`character`, `timestamptz`→`timestamp with time zone`). It strips a
`public.` prefix — **but not `pg_catalog.`**, so every rewritten type falls
through unmatched.

Measured: the two parsers' spellings converge on **3 of 12** cases today, and on
**12 of 12** once `pg_catalog.` is stripped alongside `public.`.

**Decision: fix `canonical_type` first, as its own change, before any parser
work.** It is a one-line strip plus tests. Landing it after the parser would
make every table in every project report spurious column-type drift — the same
ordering mistake the matview sentinel avoided.

It is also correct on its own merits: `pg_catalog.int4` and `integer` are the
same type today, whoever produced the string, and the introspection side can
emit either.

## Sequencing

1. **Normalizer fix.** Strip `pg_catalog.` in `canonical_type`. Standalone,
   independently useful, no parser involvement.
2. **Column-level extraction.** Columns, types, nullability, defaults, identity,
   inline PK/unique/FK. No indexes, no comments.
3. **Constraints and comments.** Table-level `PrimaryKey`/`Unique`/`ForeignKey`/
   `Check`, and `COMMENT ON COLUMN`.
4. **Indexes.** The fullest surface: access method, opclass, sort order, nulls
   ordering, expression keys, `WHERE`, `INCLUDE`, `NULLS NOT DISTINCT`, `WITH`.
5. **Switchover.** `Table` into `COVERED` and `native()`.

Steps 2–4 cannot land separately from step 5 under
`clippy --all-targets -- -D warnings` — an unwired `parse_table` is dead code, a
boundary this migration has hit four times. They are sequencing *within* one
task, not separate commits.

The matview increment already produced a minimal `IndexStmt` walker that
detects-and-skips opclass, predicate, `INCLUDE` and storage parameters. Step 4
must replace that with the full version and remove the skip, since a table's
indexes are not optional the way a matview's decoration was.

## The gate applies here, and matters most

Unlike Role (both paths became the same function) and MaterializedView (the two
paths are *intended* to differ), sqlparser produces a complete `table_def`. So
Table **can** be differentially gated — and it is the type where the gate is
worth the most, because it is the one that can lose data.

`Table` must NOT be added to `NO_SECOND_IMPLEMENTATION`. Its corpus should be
the richest of any type: composite keys, named and unnamed constraints, inline
and table-level FKs with `ON DELETE`/`ON UPDATE`, identity columns, generated
columns, partial and expression indexes, `INCLUDE`, opclasses, storage
parameters, arrays, enums, and quoted mixed-case identifiers.

Treat a gate failure here as a stop-and-report, not a fixture to adjust. Three
of this migration's real bugs surfaced exactly that way.

## Retiring the scaffolding

Once Table is native, `SqlparserDdl` has no production callers. Three things
then retire **deliberately**, in one change, rather than being left to rot:

- `preprocess_sql`'s three regex workarounds — `COMMENT ON` object types,
  `PROCEDURE`→`FUNCTION`, matview `WITH [NO] DATA`. Each exists only to make
  sqlparser swallow something Postgres reads natively.
- `extractors.rs`'s tier-3 regex text scan and the sqlparser tiers above it.
- **The parity gate itself.** With no second implementation for any type, it
  would compare each native parser against itself and report green forever — the
  failure mode already documented for Role. Deleting it is the honest end state;
  the per-type unit tests and the corpus remain.

The `source.parser` config field and `ParserChoice` also lose their meaning.
Whether to remove them or keep `sqlparser` as a rejected value with a clear
error is a follow-up decision, not part of this step.

## Risks

- **`--prune` is the sharp edge.** Any table whose extraction fails silently
  becomes an orphan. The implementation must ensure a table that cannot be fully
  read carries an **error** (so `ensure_fully_parsed` refuses) rather than a
  partial `table_def` or `None`. This is the one place where erroring is
  strictly safer than degrading.
- **Inline-vs-table-level duplication.** The sqlparser path emits a composite
  `PRIMARY KEY (a, b)` as both a table constraint *and* `is_pk` on each member
  column. `lift_pk_unique_keep_others` and `pk_unique_col_sets` depend on that
  exact shape — `pk_unique_col_sets` already documents a bug caused by taking
  those flags at face value. Reproduce the existing shape; do not rationalise it
  in this step.
- **Scale.** This is 752 lines of sqlparser extraction. It is the one step that
  may warrant splitting across sessions, and the one where a partially-correct
  result is worse than none.

## Non-goals

- No change to introspection.
- No change to the `TableDef` shape — the gate depends on the target being
  stable.
- No rationalisation of the inline-flag duplication described above.
