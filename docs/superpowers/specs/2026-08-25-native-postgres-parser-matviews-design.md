# Materialized views on libpg_query — design spec

Rollout step 2b of the native Postgres parser. Split out of step 2 during
planning because a matview stores body text, and that turned out to be a
contract question rather than a porting question.

**Parent spec:** `2026-08-24-native-postgres-parser-design.md`

## Problem

Every other entity type carries only structured facts — columns, references,
enum labels — so moving it to libpg_query is a like-for-like swap the parity
gate can verify. A materialized view carries **body text** in `writes[0]`, and
two things read it:

- `emit::emit_matview` reconstructs the `CREATE` from it:
  `CREATE MATERIALIZED VIEW … AS {writes[0]} WITH DATA;`
- `reconcile::matview_hash` hashes it (via `normalize_matview_body`) into the
  drift sentinel.

So the question is not "can libpg_query read a matview" — it can — but "what
should `writes[0]` contain", and that has consequences for existing databases.

## What `writes[0]` holds today, and why it is wrong

The sqlparser path stores `create_view.query.to_string()` — **sqlparser's own
AST re-rendering**, not the author's SQL. Measured against libpg_query's
`deparse` over 14 realistic bodies, the two renderers agree on 10; under
`normalize_matview_body`'s whitespace-and-case collapse, 12. The survivors are
paren preservation (`(data ->> 'k')` vs `data ->> 'k'`) and a space before a
paren (`bernoulli (10)` vs `bernoulli(10)`).

That 12/14 is the interesting number, not the 10/14: it is what the *runtime*
comparison would see. But both are beside the point, because storing a
re-rendering at all is the defect. `dbd` currently emits sqlparser's idea of the
user's query rather than the query the user wrote.

**Decision: `writes[0]` holds the verbatim source body.** That fixes
`emit_matview`'s round-trip, and it removes renderer disagreement as a category
rather than reducing it — there is no renderer left to disagree with.

## What the live side stores (and why it does not matter)

Worth recording, because it is counter-intuitive and cost time to establish.

`introspect_matviews` stores `pg_matviews.definition` — Postgres's canonical
rendering, which is heavily transformed. Measured on a live server:

| authored | `pg_matviews.definition` |
| --- | --- |
| `select a::text from t` | `SELECT (a)::text AS a FROM mv.t;` |
| `select coalesce(b,'x') as c from t left join u …` | `SELECT COALESCE(t.b, 'x'::text) AS c FROM (mv.t LEFT JOIN mv.u ON ((u.id = t.id)));` |
| `select a, count(*) from t group by a` | `SELECT a, count(*) AS count FROM mv.t GROUP BY a;` |

It adds aliases, parens, qualifications and casts. **No parser will ever match
it**, and none needs to: drift is not detected by comparing against it.

Drift uses a `dbd:hash=…` **comment sentinel that dbd stamps on the object
itself** (`reconcile::matview_hash`, `MatviewAction`). The comparison is
design-hash against dbd's own previous stamp. `pg_matviews.definition` is only
used to populate an introspected entity.

## The migration hazard

`matview_hash` = SHA-256 of `normalize_matview_body(entity)` plus the index key
set. `normalize_matview_body` reads `writes[0]`.

**Therefore any change to `writes[0]` changes the hash of every existing
matview**, and every dbd-managed matview in every project would report as
drifted exactly once after upgrading — indistinguishable from real drift, and
the warning tells users to `DROP … CASCADE`.

There is an irony worth preserving here: `matview_hash`'s own doc comment
explains that SHA-256 was chosen over `DefaultHasher` specifically so the stamp
is *"stable across toolchain versions and never triggers spurious drift warnings
after a Rust upgrade."* The authors guarded this exact failure mode. Changing
the parser reintroduces it through a different door.

**Decision: version the sentinel.**

```
stamp format:  dbd:hash=v2:<sha256-prefix>

Some(V2(h)) if h == want  -> Skip
Some(V1(_))               -> Restamp silently   (stamped by an older dbd)
Some(V2(_))               -> Warn               (real drift)
None                      -> Warn               (created outside dbd)
```

An unversioned stamp means "written by a dbd that hashed the old contract", so
it is re-stamped rather than reported. Upgrade becomes invisible. A `v2` stamp
that disagrees is genuine drift and still warns.

This also buys forward safety: the next contract change bumps to `v3` with the
same one-line treatment.

## Extracting the verbatim body

The AST alone is not enough, and this is the one genuinely fiddly part.

- `RawStmt.stmt_location` / `stmt_len` bound the **whole statement**
  (`create materialized view … with data`), not the body. Verified working on
  multi-statement input; `stmt_len == 0` on a trailing statement means "to end
  of input" and needs that fallback.
- The inner `SelectStmt` carries node locations, but they point at *contents* —
  `targetList[0].location` is the first column, not the `SELECT` keyword.

So the body boundaries come from the token stream. `pg_query::scan()` returns
`ScanToken { start, end, token, keyword_kind }` with byte offsets into the
original text, and — critically — it tokenizes properly, so an `as` inside a
string literal, a comment, or dollar-quoting is never mistaken for the keyword.

```
body = source[ after the `as` keyword token .. before the trailing `with` token ]
       trimmed, with a trailing `;` removed
```

**Implementation note:** `ScanToken.token` is a numeric protobuf code
(`as` = 295, `select` = 651, `with` = 747 in the version measured), not a named
variant in `Debug` output. Match on the token's **source text**
(`&sql[start..end]`, lowercased) rather than hard-coding those integers — the
tokenizer still provides the correctness guarantee, and the codes are an
internal detail that could shift with a `pg_query` bump.

Scope the token search to the statement's `stmt_location`/`stmt_len` range so a
file containing several statements cannot cross-match.

## Parity gate

Matview **cannot** be covered by the differential gate, for the same structural
reason as Role but arriving differently: here the two implementations are
*intended* to produce different `writes[0]` — a re-rendering versus verbatim
source. A gate asserting they agree would assert the change did not happen.

Add `MaterializedView` to `NO_SECOND_IMPLEMENTATION` in
`tests/parser_parity.rs`, with that reason stated, when it goes native.

Its correctness rests instead on:

- unit tests over body extraction, including comments, dollar-quoted strings, an
  `as` inside a string literal, `WITH NO DATA`, no trailing semicolon, and a
  trailing `CREATE INDEX`;
- a round-trip test: parse a file, `emit_matview`, and assert the body survives
  **verbatim** — the property the old contract could not hold;
- live verification that a matview applies, reconcile converges, and a v1-stamped
  matview is silently re-stamped rather than warned.

## Non-goals

- No change to `pg_matviews.definition` handling on the introspection side.
- No change to how matview drift is *resolved* — dbd still warns rather than
  auto-recreating, because that would `DROP … CASCADE` and lose data and
  dependents.
- Plain views are already native and unaffected.

## Risks

- **Body extraction is the only novel mechanism in the whole migration.** Every
  other type reads structured AST fields; this one slices source text. The
  token-scoping and the `stmt_len == 0` fallback are where bugs will be.
- **The v1→v2 re-stamp is a write.** It changes a comment on a live object
  during what a user may think is a read-only operation. It must not fire under
  `--dry-run`; confirm that against `MatviewAction` handling.
- A project that has never run this version has no v2 stamps, so the first
  reconcile after upgrade re-stamps every matview. That is one write per matview,
  once — acceptable, but it should be visible in the summary output rather than
  silent.
