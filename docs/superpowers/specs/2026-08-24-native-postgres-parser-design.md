# Native Postgres parser — design spec

Replace sqlparser with libpg_query (`pg_query`) as dbd's DDL parser for the
Postgres dialect, behind a `DdlParser` trait, keeping sqlparser for other
dialects.

## Problem

`parser::parse_entity` reads every DDL file with `sqlparser` under a hardcoded
`PostgreSqlDialect` (`parser/mod.rs:174`). sqlparser is a hand-written,
multi-dialect reimplementation of SQL grammar, so it lags the Postgres server.

Measured over a 24-construct corpus of realistic Postgres DDL, sqlparser 0.62
rejects 4 that Postgres accepts:

| construct | sqlparser error |
| --- | --- |
| `create function … window` | `Expected: end of statement, found: window` |
| `exclude using gist (r with &&)` | `Expected: ',' or ')' after column definition, found: gist` |
| `create unlogged table t (…)` | `Expected: an object type after CREATE, found: unlogged` |
| `… with cascaded check option` | `Expected: end of statement, found: with` |
| `trim(both from a)` | `Expected: ), found: a` |

libpg_query rejects **0 of 24**: it is Postgres's own C parser, vendored from
the server, so its grammar is identical by construction to the version it is
built from (PG 17 here).

The gap is not cosmetic. A parse error drops the entity from apply/reconcile's
desired set, which produced a false green — `dbd apply` reporting "N entities
applied" and exiting 0 while never creating the object. That symptom is now
guarded (`Design::ensure_fully_parsed`) and partly mitigated (a libpg_query
validation fallback for Function/Procedure/View), but both are workarounds for
using the wrong parser. This spec removes the cause.

sqlparser is also loose in the other direction: it *accepts* `SELECT FROM FROM`
and `… where where`, which Postgres rejects — so a file can pass dbd's parser
and still fail at apply.

Three regex workarounds in `preprocess_sql` and one regex scanner
(`extract_role_memberships`) exist purely because sqlparser cannot read
Postgres. They retire as this work lands.

## Goals

- Postgres DDL is parsed by Postgres's own grammar.
- Every construct Postgres accepts produces a usable `Entity`.
- Migration is incremental and continuously releasable — no long-lived branch.
- Compatibility is proven, not asserted, before each entity type switches over.
- Users have an opt-out if the new parser misbehaves.

## Non-goals (scope boundaries — YAGNI)

- **The formatter keeps sqlparser.** `formatter/river.rs` + `create.rs` (~73
  refs) render SQL for `dbd format`; that is a different job from semantic
  extraction. See Follow-ups.
- **No change to `Entity` / `TableDef` shape.** The differential test depends on
  the target type being stable.
- No new dialect support. The non-Postgres branch keeps today's behaviour.
- Unrelated known gaps stay out: view `CREATE OR REPLACE` incompatible changes,
  and `dbd inspect` exiting 0 when it reports errors.

## Architecture

```rust
// parser/mod.rs
pub(crate) trait DdlParser: Sync {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity>;
}

pub enum ParserChoice { Sqlparser, PgQuery }

impl ParserChoice {
    /// `source.parser` wins when set; otherwise the dialect decides.
    /// An unrecognised `source.parser` is an error, not a silent fallback.
    pub fn resolve(dialect: &str, explicit: Option<&str>) -> Result<Self>;
}
```

Dispatch:

| `source.parser` | `source.dialect` | parser |
| --- | --- | --- |
| `pg_query` | any | `PgQueryDdl` |
| `sqlparser` | any | `SqlparserDdl` |
| unset | `postgresql`, `postgres`, `supabase` | `PgQueryDdl` |
| unset | anything else | `SqlparserDdl` |
| unrecognised value | — | config error |

### Config

`source.parser` is new **public API**, so it is validated at load:

```yaml
source:
  dialect: postgresql
  parser: sqlparser    # optional: pg_query | sqlparser
```

Typed as `Option<String>` on `SourceConfig`, rejected at `Design::from_config`
with the accepted values named in the message.

### Threading

`parse_entity` is called from exactly one production site
(`design/mod.rs:400`), where the config is already in hand. Two entry points:

- `parse_entity_with(choice, file, sql)` — the real one, used by the scan loop.
- `parse_entity(file, sql)` — Postgres-default wrapper, so the `emit.rs` and
  `dbml_parse.rs` round-trip callers stay untouched.

### Layout

Additive; no existing files move.

```
parser/
  mod.rs            trait, ParserChoice, dispatch, parse_entity(_with)
  extractors.rs     unchanged (sqlparser)
  tables.rs         unchanged (sqlparser)
  sqlparser_ddl.rs  thin DdlParser impl wrapping today's parse_entity body
  pg/               NEW
    mod.rs          PgQueryDdl + COVERED
    enums.rs
    views.rs
    procs.rs
    tables.rs
```

The libpg_query helpers already in `extractors.rs` —
`extract_enum_values_via_pg_query`, `extract_search_paths_via_pg_query`,
`extract_view_refs_via_pg_query`, `is_valid_postgres` — move into `parser/pg/`
as its seed.

## Rollout

`PgQueryDdl::parse` matches on entity type and delegates anything not yet
covered to `SqlparserDdl`. A single constant is the source of truth:

```rust
impl PgQueryDdl {
    /// Types parsed natively. Drives both dispatch and the parity test, so the
    /// two cannot drift.
    const COVERED: &'static [EntityType] = &[/* grows per step */];
}
```

| # | type | rationale | retires |
| --- | --- | --- | --- |
| 1 | Enum | seed already written; smallest surface | the enum arm of the libpg_query fallback |
| 2 | View | `select_tables()` + `call_functions()` | — |
| 2b | MaterializedView | **split out — needs its own spec**, see below | matview `WITH DATA` regex workaround |
| 3 | Function / Procedure | see the measured note below | `PROCEDURE`→`FUNCTION` regex workaround |
| 4 | Role | libpg_query parses `GRANT role TO role` natively | `extract_role_memberships` regex |
| 5 | Table | 752 lines, highest risk, most surface | `COMMENT ON` regex workaround |

**Step 3 is larger than "the `parse_plpgsql` tier already exists" implies.**
Measured across 7 routine bodies: the libpg_query tier agrees with the incumbent
on all 3 PL/pgSQL cases and none of the 4 `LANGUAGE sql` cases, because
`extract_proc_refs_via_pg_query` only reads PL/pgSQL blocks. A native parser
needs a second path that parses a `LANGUAGE sql` body with `pg_query::parse` and
reads `select_tables`/`dml_tables`/`call_functions` from it.

It must also reproduce a deliberate semantic rule in the incumbent: called
functions are collected for `LANGUAGE sql` bodies only. Postgres validates those
at creation (`check_function_bodies = on`), so a function they call must exist
first; a PL/pgSQL body resolves names at run time, so its calls are not
creation-order dependencies. That means detecting the language from
`CreateFunctionStmt`'s options rather than treating all bodies alike.

Noted while measuring: `extract_proc_refs_via_pg_query` returns `Some(([], []))`
rather than `None` for a non-PL/pgSQL body, which makes the regex tier below it
unreachable. Latent today — tier 1 catches those bodies first, and in the case
tested the regex tier would also have found nothing — but the tier chain does
not fall through as its own doc comment describes.

**Step 2 was split during planning.** A matview stores its body in `writes[0]`,
which `emit_matview` and `reconcile` use to rebuild the `CREATE`. The sqlparser
path fills it with `query.to_string()` — sqlparser's own re-rendering — and
libpg_query's deparse agrees on only 10 of 14 realistic bodies (`a::TEXT` vs
`a::text`, `coalesce` vs `COALESCE`, paren preservation, `TABLESAMPLE BERNOULLI
(10)` vs `bernoulli(10)`). Byte parity is unreachable without first deciding
whether `writes[0]` should hold verbatim source sliced by
`RawStmt.stmt_location`/`stmt_len` instead — a change that alters `emit` output.
A plain view carries only references, so it is parity-clean and ships alone.

Step 5 covers columns, types, defaults, identity, PK/unique/FK, CHECK, indexes
(access method, opclass, predicate, include, storage params) and comments. It is
expected to need its own spec and plan; the earlier steps deliberately prove the
trait shape on smaller surfaces first.

Once every type is covered, the libpg_query *validation fallback* added in
`parse_entity`'s error arm becomes dead code for Postgres and is removed.

## Testing

Both layers are required: neither covers the other's failure class.

### Differential parity — `tests/parser_parity.rs`

Runs both parsers over every `.ddl`/`.sql` in `tests/fixtures/`, plus a new
`tests/fixtures/parser_corpus/` holding one file per construct.

```
for each file whose EntityType ∈ PgQueryDdl::COVERED:
    old = SqlparserDdl.parse(file, sql)
    new = PgQueryDdl.parse(file, sql)

    if old.errors.is_empty():
        assert json(old) == json(new)     // no regression
    else:
        assert new.errors.is_empty()      // improvement
```

Two details that make this real rather than decorative:

- **Restricting to `COVERED` is load-bearing.** A delegated type would compare
  `SqlparserDdl` against itself and pass for free — a green test proving
  nothing.
- Comparison is on `serde_json::to_value`, because `Entity`, `EnumValue`,
  `Reference` and `TableConstraint` do not derive `PartialEq`. This also yields
  a readable field-level diff on failure and avoids touching the core type.

### Per-type TDD

Unit tests for the constructs sqlparser cannot parse — `setof`, `variadic`,
`window`, `exclude`, `unlogged`, `with check option`, `DO`-guarded enums —
asserting the expected `Entity` directly. There is no old output to diff
against, so parity cannot cover these.

A type joins `COVERED` only when both layers are green for it.

### The gate cannot outlive the second implementation

Differential parity only proves something while two independent
implementations exist to disagree. Role was the first type to lose that:
sqlparser cannot parse `DO $$ … $$` at all, so its "incumbent" was never
sqlparser but a regex scanner, and that regex was deleted rather than kept
alongside the native parser — keeping it would have left `source.parser:
sqlparser` selecting the *worse* implementation on purpose. `SqlparserDdl`
now delegates `EntityType::Role` to the same `pg::roles::parse_role` the
native path calls, so a Role file in the corpus would compare that function
against itself. That was verified directly: mutating `parse_role` to drop a
membership left `parser_parity.rs` green. `tests/parser_parity.rs` now lists
Role in `NO_SECOND_IMPLEMENTATION` and skips it explicitly rather than
counting it as covered; Role's correctness rests on its unit tests
(`parser::pg::roles`) and on live verification instead.

Table and `MaterializedView` will hit the same thing when sqlparser is
finally removed at the end of this migration — at that point every type
delegates to itself and the gate retires entirely, rather than silently
self-comparing every type it lists.

## Error handling

`PgQueryDdl` records a parse error only when libpg_query itself rejects the
file — that is the definition of invalid Postgres. This preserves the invariant
`Design::ensure_fully_parsed` depends on: an entity carries an error only when
the file is genuinely unreadable, so apply/reconcile refuse only on real
breakage.

**Error messages lose line and column.** libpg_query's C API reports a
`cursorpos`, but the Rust binding keeps only `error.message` and discards it
(`pg_query::query::parse` builds `Error::Parse(message)` from
`(*result.error).message` alone). Measured on the same invalid input:

| parser | message |
| --- | --- |
| sqlparser | `Expected: identifier, found: ; at Line: 1, Column: 29` |
| libpg_query | `syntax error at or near ";"` |

The offending token is still named, which is the bulk of the diagnostic value,
but `dbd inspect` output gets less precise. This is an accepted regression for
this rollout and tracked as F5.

## Risks

- **AST verbosity.** libpg_query's protobuf tree is lower-level than
  sqlparser's enums, so `parser/pg/` will exceed the LOC it replaces. Mitigated
  by one module per entity type.
- **Table extraction is the bulk of the work** and the one with real safety
  consequences: a missing `table_def` filters the table out of the desired
  snapshot (`reconcile::raw_snapshot_from_entities`), making a live table read
  as an orphan that `--prune` would drop. It ships last, behind the parity gate.
- **PG version coupling.** libpg_query's grammar is that of the Postgres it
  vendors. A server newer than the bundled parser may use syntax it rejects —
  the same class of gap, much narrower. Bumping `pg_query` is the remedy.

## Follow-ups (out of scope here)

### F1 — Formatter on libpg_query

`formatter/` is the last significant sqlparser consumer. Retiring it needs a
different tool than the parser did, and one trap is worth recording now:

**`pg_query::deparse` is the wrong tool.** It renders Postgres's *canonical*
form and discards the author's layout entirely, which is the opposite of what a
formatter must do.

**`pg_query::scan` is the right one.** Postgres's own lexer, returning
`ScanToken { start, end, token, keyword_kind }` — byte offsets into the original
text, keyword classification (`ReservedKeyword` / `UnreservedKeyword` /
`TypeFuncNameKeyword` / `NoKeyword`), and comment tokens (`CComment`,
`SqlComment`). Tokens with positions and trivia are better formatter input than
an AST: `keyword_case` becomes a direct read of `keyword_kind` rather than a
keyword allowlist (`formatter/keyword_case.rs`), and alignment can work from
real spans.

Scope when picked up: `formatter/river.rs` (SELECT alignment),
`formatter/create.rs` (CREATE rendering), `formatter/keyword_case.rs`, and
`formatter/split.rs` — the last of which `pg_query::split_with_scanner` may
replace outright. The acceptance bar is that formatting every fixture is a
no-op diff against today's output, except where today's output is provably
wrong.

### F2 — Retire the remaining regexes

`preprocess_sql`'s three workarounds and `extract_role_memberships` retire with
rollout steps 2–5. Track that the file's `WORKAROUND_REGISTRY` header block
(`parser/mod.rs:20-38`) is deleted, not left describing removed code.

### F3 — Unconditional validation

The libpg_query validation added in `parse_entity`'s error arm only runs when
sqlparser *fails*. Running it unconditionally would also catch the opposite
gap — files sqlparser accepts that Postgres rejects (`SELECT FROM FROM`,
`where where`). Once `PgQueryDdl` is primary this is moot for Postgres, but it
remains open for whatever still routes to `SqlparserDdl`.

### F5 — Recover line and column in libpg_query parse errors

See Error handling: the binding drops `cursorpos`, so native parse errors name
the token but not where it is. `pg_query::bindings` exposes the raw
`PgQueryError` struct, so a small wrapper could read `cursorpos` and convert the
byte offset into line/column against the source text. Worth doing once several
entity types are native and `dbd inspect` output is mostly libpg_query's.

### F4 — Separately-tracked, not caused by this work

- View `CREATE OR REPLACE` incompatible changes (reorder/rename/drop/retype a
  column) abort reconcile mid-run with Postgres's raw error. Matviews already
  get a plan-time warning naming the `DROP … CASCADE` to run; plain views get
  nothing. Mirror the matview precedent — warn, do not auto-drop.
- `dbd inspect` exits 0 while reporting errors, so it cannot gate CI.
