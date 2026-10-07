# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html); while
the crates are `0.x`, the **minor** position is the breaking one, so
`0.12.x → 0.13.0` may require changes in code that embeds `dbd-core`.

## [Unreleased]

**Commands now do what the documentation says.** An audit of the guides
against the code found the reverse cases too — behaviour the docs promised
that the code never had — and those are fixed here, in the code. Two of them
were destructive: a database deployed with `-e production` was not protected
by reset's prod guard, and a Supabase project's `reset --schemas` dropped
`public`.

### Changed

- **`-e` takes `dev` or `prod`** (and the aliases `development` and
  `production`), and refuses any other name. Aliases are normalised where they
  are parsed, so bookkeeping records `prod`, never `production`. A name like
  `-e staging` used to be accepted and then matched no `import/<env>/` folder.
- **`dbd reset --target` defaults to the design's target** — its first
  `target:` key — instead of `postgres`. Pass `--target` to override.
- **Library: grants moved into core.** `Design::apply_grants` applies the
  design's schema and target grants, and `Design::deploy` calls it;
  `DeployComplete` gains a `grants: GrantsOutcome` field. Code that builds a
  `DeployComplete` literal needs the field (or `..Default::default()`).

### Fixed

- **A database deployed with `-e production` is protected by reset's prod
  guard.** The guard matched `prod` only, and `-e production` was recorded
  verbatim, so `dbd reset` ran against it. Existing rows that say `production`
  are now read as prod too.
- **`dbd reset --schemas` on a Supabase project keeps `public`.** The protected
  schema set came from `--target`, which defaulted to `postgres` whatever the
  design targeted, so `public` was dropped `CASCADE` unless the flag was given.
- **`dbd deploy` applies grants.** They ran only inside `dbd apply`'s handler,
  so a deploy — and every embedder calling `Design::deploy` — got a schema with
  no grants and no warning. They now run between the schema and the data.
- **`dbd deploy` honours `-c`.** It read `design.yaml` whatever `-c` named.
- **`dbd emit` carries foreign keys, CHECK constraints and indexes.** They were
  dropped without a report entry, though the report is the only safeguard
  `emit` offers. A CHECK expression or an index predicate passes through
  untranslated and is reported; an index the target would reject — an
  expression key on SQL Server, a key on an unbounded text column on MySQL or
  SQL Server — is left out and reported, so the script still applies.
- **`GITHUB_TOKEN` authenticates GitHub downloads**, so `dbd deploy` can fetch a
  private repository. It was documented and never sent. It goes to
  `api.github.com` only.
- **A GitHub `--source` outside `dbd deploy` is refused with the way out.**
  Only `deploy` downloads a source; other commands failed trying to read
  `owner/repo/design.yaml` from disk. They now say to clone the repository and
  pass its path.
- **`dbd init --target` refuses a target it has no scaffold for.** `convex`,
  `sqlite` or a typo silently produced a PostgreSQL project.
- **`dbd doctor --fix` migrates every folder alias the scanner reads**:
  `materialized_views`, `matview`, `matviews` and `sequences` were scanned but
  never moved to their canonical folder.
- **The viewer refuses a schema model newer than it reads**, saying so, instead
  of rendering it with whatever the new version added silently dropped.
- **`--help` for `--source` and `dbd diff`** no longer claim a GitHub source
  works everywhere, or that reconcile skips CHECKs and comments.

## [0.24.1] — 2026-10-06

**A data-loss fix.** `dbd reconcile --prune --scope` could drop tables the design
declares, whenever they were outside the scope but in a schema the scope touches.
Every release with `--scope` is affected; upgrade before running a scoped prune.

### Fixed

- **`reconcile --prune --scope` no longer drops the project's own tables.**
  Under a scope, a table the design declares outside it sat in a managed schema
  without being desired, read as an orphan, and was dropped — `DROP TABLE …
  CASCADE` on the project's own data; `dbd diff --scope` reported the same drop.
  A scope now narrows what is applied, not what the project owns: what the
  design declares outside the scope is hidden from the live side, so it is
  neither altered nor pruned, while anything the design does not declare at all
  is still an orphan. Unscoped runs are unchanged. ([#40])

[#40]: https://github.com/sensei-hq/dbd/issues/40

## [0.24.0] — 2026-10-03

**The viewer documents the schema, and remembers it.** It opens on a project
overview ([#28]); every entity — tables, views, routines, triggers and enums — is
in the sidebar, filterable by type, and opens a page of its own ([#27], [#34]); and
a changelog shows what every snapshot changed, for the project and for each
table and enum ([#29], [#33]). The changelog is the part that reaches Rust:
`dbd diagram` now reads `snapshots/`, and the schema model moves to **v3**,
carrying `history` and `enums`. First, the diagram got its controls back ([#25]):
`@rokkit/graph` 1.7 made `Graph` a bare canvas and changed its default layout,
so the 0.23.0 viewer drew a different diagram with no controls and no visible
schema; the site is now on rokkit 1.9.0.

**For embedders: a minor, and a breaking one in the 0.x sense.** `SchemaModel`
has two new public fields, `history` and `enums`, so code that builds one as a
struct literal must name them (`history: vec![], enums: vec![]`); and
`MigrationGraph` gains `stage`. The JSON is additive — each is omitted when
empty, and a v2 payload still deserializes.

### Added

- **A changelog, from snapshots** ([#29]). `dbd_core::history::load` turns
  `snapshots/NNN.json` into one entry per version: the first a baseline of
  table and enum counts, each later one the tables and enums it added, removed
  or modified, with their column, index, constraint and enum-value edits. It
  reuses `diff::diff` and `classify_changes`, so a step reads the way its
  migration did; a rename or type change cut in stages is one version that reads
  as the rename or type change (never the synthetic `_new` column); type
  spellings are canonicalised; output is sorted. `dbd diagram` attaches it
  (scoped under `--scope`), and an unreadable snapshot costs the changelog with
  a warning, not the diagram. The viewer's **Changelog** tab shows it newest
  first, a card per version. Views and routines have no history yet — snapshots
  hold tables and enums only.
- **Every entity in the sidebar, filterable by type** ([#34]). Views,
  materialized views, functions, procedures, triggers and enums are listed
  beside the tables in their diagram icons, with a toggle per kind and search
  within the kinds that are on. A view or routine opens a page of what it uses
  and what uses it, with its dependency neighbourhood; an enum, its values and
  the columns of its type. `SchemaModel` v3 gains `enums` (values in order and
  the comment), and the parser now keeps an enum's `COMMENT ON TYPE`, which it
  parsed and dropped.
- **Each table and enum has its own changelog** ([#33]): the versions that
  changed it, newest first, with the edits inside — the project changelog
  narrowed to one entity. One that no version touched has been there since the
  baseline.
- **A project overview** ([#28]) — the page the viewer opens on: database and
  model version, a tile per count (schemas, tables, each entity kind present,
  enums, references) in its diagram icon, the project note in full, the counts
  per schema, and the latest three versions from the changelog.
- **A Details tab per table** ([#27]): table info (the comment as markdown), then
  Fields — Name, Type, Settings, Default, References, Notes, with notes rendered
  as markdown — then references in and out, the views and routines that read or
  write the table, and indexes. Full width; the fields table scrolls sideways
  rather than squeezing its notes.

### Changed

- **`SchemaModel` is version 3**, adding `history` and `enums`.
- **rokkit 1.9.0** (from 1.7.0). The entity diagram is a plain `Neighborhood`
  again — the package now tints by schema and withholds the states a focus
  selection made meaningless (rokkit#172), so dbd no longer owns its state; a
  clicked focus is the one card marked selected. Zoomed content is fully
  reachable and zoom holds its place (rokkit#171), and a neighbourhood centres
  its drawn cards whichever side is empty (rokkit#170). `@rokkit/ui`'s widened
  `shiki` peer clears the install warning carried since 0.23.0.
- **`graph.json` records each version's stage** — `"stage": {"index", "of"}`,
  `1/1` for an ordinary version — so the changelog groups a multi-stage change by
  structure, not by a description a person may have typed `(stage 1/2)` into.
  Graphs written before it have none and still read.
- **The viewer opens on Overview**; the tabs are Overview · Diagram · Entities ·
  Changelog.
- **`/diagram` and `/projects` use the home page's theme switcher**
  (`@rokkit/app`'s `ThemeSwitcherToggle`) instead of a page-local moon/sun
  button — one colour-mode control across the site.

### Fixed

- **The ER diagram composes `ErDiagram`**: `flow` ranking (every edge leaves a
  card's right side and enters the next one's left), the schema painted as a
  spine on each card and keyed in a legend under the canvas, and the density,
  edge-style and zoom controls over it. `arrange` is gone — it ordered
  `cluster`'s boxes, and `flow` has none.
- **The entity diagram composes `Neighborhood`**, with depth, edge-style and
  zoom controls, in the root's style: schema tint on every card and no
  selection highlight. A selected focus had outlined every neighbour as
  related and dimmed the second ring to 30% — the ring a reader asked to see.
- **A qualified name sits on one line.** `auth.users` read as `auth.` above
  `users` in the entity header and the entities list.
- **The "click a table" hint moved to the top of the canvas**; the controls and
  legend own the bottom edge, and it overlapped the legend.
- **The header stays put on the Overview tab.** It dropped its subtitle and
  stats there; it now shows the note's first paragraph and the stats on every
  tab, and the overview's Notes renders what follows.
- **A comment naming the same `code` twice no longer crashes the page.** Inline
  code was keyed by its own text (Svelte's `each_key_duplicate`) in the entities
  list and the Details tab; every comment now goes through one renderer.

[#25]: https://github.com/sensei-hq/dbd/issues/25
[#27]: https://github.com/sensei-hq/dbd/issues/27
[#28]: https://github.com/sensei-hq/dbd/issues/28
[#29]: https://github.com/sensei-hq/dbd/issues/29
[#33]: https://github.com/sensei-hq/dbd/issues/33
[#34]: https://github.com/sensei-hq/dbd/issues/34

## [0.23.0] — 2026-09-29

**The viewer stopped being dbd's to maintain.** 0.22.0 shipped schema model v2
and noted the viewer was "being extracted into a package dbd, sensei and Rokkit
share". This is that extraction arriving from the other side: the ER diagram now
renders through `@rokkit/graph`, and dbd's own layout engine — clustering, edge
routing, card geometry and their tests — is deleted. 921 lines net, none of which
described anything specific to dbd.

**The Rust tree is byte-identical to 0.22.0.** `crates/`, `Cargo.toml` and
`Cargo.lock` carry no change; `dbd-core` and `dbd-cli` 0.23.0 are 0.22.0
republished under a new number. By the rule 0.13.1 stated — site work that leaves
the Rust tree untouched carries no bump of its own — this release exists because
the website and the CLI are versioned together, not because the crates moved.
Nothing installed from crates.io behaves differently.

Found while building it: **`@rokkit/graph` was never a dependency.** It was a
`link:` to a sibling checkout on one machine, so CI and the Cloudflare build
would have resolved nothing the moment the diagram started importing it.

### Changed

- **The ER diagram renders through `@rokkit/graph`** rather than dbd's own
  layout code. `DiagramView` and `EntityDiagram` call `Graph` and
  `toGraphInput`; `layout.ts`, `layout-clusters.ts`, `layout-edges.ts`,
  `layout-types.ts` and `layout-cards.ts` are gone along with their tests.

  The layout maths was never dbd-specific — it clustered nodes by schema and
  routed edges between cards, which is what any node-link diagram does. Keeping
  a private copy meant dbd owned a general-purpose renderer as a side effect of
  wanting to draw one schema. The page tests assert the same property through
  the package's `[data-graph-node]` hook instead of the retired `[data-card]`:
  a card per table, unchanged in intent.

- **Every `@rokkit/*` package 1.4.1 → 1.7.0, all ten resolved from the
  registry.** `@rokkit/graph` moves off `link:` to `^1.7.0` now that it is
  published.

- **The `SchemaModel` TypeScript mirror tracks v2** — `version`, `entities`,
  `deps`, and `fk`/`uq` on `Column`, matching `schema_model.rs` as of 0.22.0.

  Every v2 addition is optional in the mirror, which is what keeps an old share
  link working: a fragment encoded against v1 still validates and still renders.
  The mirror stays hand-maintained in dbd on purpose — `@rokkit/graph` does not
  import it, so the package never becomes a third definition to hold in step.

### Fixed

- **Two `@rokkit/*` packages resolved through symlinks into a local checkout,
  and only one of them said so.** `@rokkit/graph` was declared
  `link:@rokkit/graph` — honest about being a link, but pointing at
  `~/Developer/rokkit`, a path that exists on one machine. It is what renders
  the diagram, so CI and the deployed build would have had nothing to import.

  `@rokkit/themes` was worse for being quiet: `package.json` asked for `^1.4.1`
  while `node_modules` served 1.7.0 from that same checkout. Local builds and CI
  were resolving different code with nothing in the tree to indicate it. Both
  links are gone; all ten packages now carry integrity hashes in `bun.lock`.

- **Runtime-selected node icons were purged from the stylesheet.**
  `@rokkit/graph` picks a node's icon from its kind at runtime, so the class
  names appear nowhere in source and UnoCSS's extractor dropped them — the
  glyph vanished and each card rendered a blank box where its kind should be.
  `uno.config.ts` now safelists `DEFAULT_ICONS`, which the package exports
  precisely so the list is not hand-maintained downstream.

## [0.22.0] — 2026-09-27

**The schema model became a model of the schema.** `dbd diagram --json` emitted
tables and foreign keys and nothing else — `build` filtered
`entity_type == Table` behind a doc comment calling itself "an extension point
for view/function/procedure later". v2 adds views, materialized views,
functions and procedures, plus the dependency graph an ER diagram cannot show:
what reads, writes and calls what. It states its own `version` now, because it
is read by dbd's viewer, by the package being extracted for sensei and Rokkit,
and by anything pointed at the JSON.

Found while building it: **`COMMENT ON` was captured for tables and nothing
else.** A comment lives on `TableDef::comments`, a view or routine has no
`TableDef`, so its comment parsed cleanly and was dropped — blanking every
non-table row of the entity description table the diagram generates.

Also in this release, both carved out of #7: a schema can declare itself
**exposed** (PostgREST-served) so `dbd inspect` can report exposed tables with
no RLS policy, and **`reconcile --prune` no longer drops what the platform
owns** — the one operation that could destroy an object the project never
declared, previously unprotected.

**Breaking:** `config::SchemaGrantConfig` is renamed `SchemaOptions`.
`SchemaModel` and `Column` gained fields, so struct literals in embedding code
need updating; deserialization is unaffected (the new fields carry serde
defaults).

### Added

- **Schema model v2 — views, routines, the dependency graph, and comments
  everywhere** (#24). `dbd diagram --json` was tables and foreign keys, and
  nothing else: `build` filtered `entity_type == Table`, and `TableNode.kind`
  was hardcoded `"table"` behind a doc comment calling itself "an extension
  point for view/function/procedure later". Anything rendering from that JSON
  could draw an ER diagram and stop.

  The model now states a `version` (`2`) and carries:

  - `entities` — views, materialized views, functions and procedures;
  - `deps` — what reads, writes or calls what, the call/reference graph the ER
    diagram could never show;
  - `fk`/`uq` on each column, so a renderer picks a glyph without re-scanning
    `refs` and the index list to work out what the column is.

  `tables` and `refs` keep their exact v1 shape and contents. Folding every
  kind into one array under a `kind` discriminator would read tidier and would
  silently change what every existing consumer of `tables` receives — at the
  moment the viewer is being extracted into a package dbd, sensei and Rokkit
  share. `version` is there so the next extension is not a guess downstream.

  `deps` is a projection of `Entity::refs`, already resolved and deduplicated:
  no second parse. An edge whose target is not a project entity — a built-in
  like `now()`, or a genuine dangling reference — carries `"unresolved": true`
  rather than being dropped. The edge is real; only the endpoint cannot be
  placed, and dimming it beats pretending the call does not happen.

- **`COMMENT ON` is captured for views, materialized views, functions and
  procedures** (#24). A table's comments live on `TableDef::comments`. Nothing
  else has a `TableDef`, so `COMMENT ON VIEW recent IS '…'` parsed cleanly
  through libpg_query and was dropped on the floor — confirmed by probing all
  three parsers, every one reporting `comment=None` on a file that plainly has
  one.

  Not cosmetic: the diagram's entity description table is built from comments,
  so every non-table row in it came out blank. New `Entity::comment`, captured
  by one shared helper and projected to `note` (first line) / `noteMd` (full)
  exactly as a table's comment already is. Tables deliberately keep
  `TableDef::comments` as their single source of truth rather than gaining a
  second home for the same string.

- **Exposed vs internal schemas** (#7). A schema can declare whether something
  outside the database serves it — PostgREST on Supabase:

  ```yaml
  schemas:
    - app:
        exposed: true
    - internal          # private: the default
  ```

  Internal unless declared, except `public` on a `supabase` target, mirroring
  Supabase itself. An explicit `exposed: false` overrides that default; a bare
  schema name does not, because saying nothing is not the same as saying no.
  Nothing is exposed on a plain `postgres` target.

  **What it buys:** `dbd inspect` now reports every table in an exposed schema
  with no RLS policy declared — on Supabase, tables readable by `anon` over
  HTTP. Advisory only, never changing the exit code, because dbd cannot know
  the author did not mean it. A policy is a file at
  `policies/<schema>/<table>.sql`; dbd checks one exists rather than parsing
  it.

  This is a **different axis** from the Supabase protected set, which is about
  who *owns* a schema. `extensions` is Supabase's and not exposed; a project's
  own schema can be exposed without being Supabase's. Ownership decides what
  `reconcile --prune` may drop; exposure decides what the internet can read.
  Conflating the two in one list is what left both questions unanswerable.

  `Design::exposed_schemas()` and `Design::unprotected_exposed_tables()` are
  public. `config::SchemaGrantConfig` is renamed `SchemaOptions` — it carries
  more than grants now.

### Fixed

- **`reconcile --prune` no longer drops what the platform owns** (#7). Prune
  is the only dbd operation that drops an object the design does **not**
  declare — `reset` emits a `DROP` per *declared* entity, and its protection
  covers the schema object alone. So prune is the only one that can destroy
  something the project never knew about, and it had no protection at all.

  On a Supabase target that bites: one project table declared in `auth` puts
  the whole schema into `managed_schemas`, every table Supabase keeps there
  reads as an orphan, and prune drops the lot. Recreating them loses the
  policies and grants the environment runs on.

  `SUPABASE_PROTECTED` could not express the fix, because it conflated two
  properties. It is now two lists:

  - `SUPABASE_INFRASTRUCTURE` — `auth`, `storage`, `vault` and the rest.
    Supabase owns them and the project owns nothing in them, so prune never
    touches their contents.
  - `SUPABASE_PROTECTED` — the above **plus `public`**, protected from
    `DROP SCHEMA` because recreating it loses grants, while its *contents*
    stay the project's. `public` is still prunable, and reset still drops
    entities in it.

  The filter applies to the **drops**, not to `managed_schemas`. Narrowing the
  managed set would also narrow the live snapshot, so a table the project
  legitimately keeps in `auth` could be created and then never checked for
  drift again — a blind spot traded for the fix. Filtering drops keeps every
  declared object fully reconciled and removes only the dangerous operation.

- **`Design::target_name()`** is public — which schemas belong to the platform
  rather than the project depends on it.

## [0.21.0] — 2026-09-26

**`dbd emit`** translates a PostgreSQL schema into MySQL, T-SQL or SQLite DDL.
Anything the target cannot express is downgraded to the nearest equivalent and
reported — inline in the file, in the run summary, and as JSON for CI. A
faithful mapping is reported nowhere, so the report stays worth reading.

Two correctness fixes behind it. **`reconcile` finally converges** (#18): it
compared defaults as text, and PostgreSQL rewrites some of them on store, so a
freshly applied design reported drift forever. Measuring both sides on a live
server found more than the report described — the timestamptz form is rendered
in the *session's* timezone, and `'now'`/`'today'` are frozen at DDL time and
can never converge, so they stay visible as drift rather than normalised to an
invented value.

And the **non-PostgreSQL dialects are honest about being read-only**. `diff`
and `reconcile` reported "in sync" for T-SQL and MySQL projects — the guard
added for SQLite asked `parser == Verbatim`, and both were added after it. A
`mysql://` URL failed with a *PostgreSQL* pool timeout, because `connect` fell
through to Postgres for any unrecognised scheme.

**Breaking:** none to the library API. `dbd emit` is new.

### Added

- **`dbd emit --dialect mysql|tsql|sqlite`** — the schema as another engine's
  DDL ([#23]). Distinct from `combine`, which consolidates *this* project's own
  DDL into one script; `emit` translates it for a different engine.

  ```sh
  dbd emit --dialect mysql -f schema.mysql.sql --report downgrades.json
  ```

  **One direction, by construction.** Only the PostgreSQL reader produces
  columns and constraints (`ParserChoice::produces_structure`), so emitting
  *from* a T-SQL, MySQL or SQLite project is refused rather than quietly
  producing an empty schema. Tables and views; routines are skipped and
  reported, because their bodies do not translate.

  **Downgrade, never drop, never refuse.** A construct the target cannot
  express becomes the nearest thing it can, so the output is always a complete
  schema. Every *lossy* downgrade is reported three ways — a comment at the
  site in the emitted file, the run summary, and `--report` as JSON for CI to
  assert on. A faithful mapping (`integer` → `INT`) is reported nowhere, or the
  report becomes noise nobody reads.

  ```sql
  -- dbd: `tags` was `text[]` — emitted as JSON; no array type exists here, so
  --      the `text` elements become a JSON document
  `tags` JSON,
  ```

  The exit status stays 0 when downgrades happen: they are the documented
  behaviour, which is why the count — not the exit code — is what a pipeline
  should assert on.

- **A view keeps its body.** `Entity::body` now carries a view's `SELECT`, as a
  materialized view's always did. The view parser deliberately omitted it, with
  a comment explaining that nothing rendered a view's body and the omission
  kept it "parity-clean against the incumbent" — the incumbent being the
  sqlparser path retired in 0.14.0. `emit` renders one, and without this a view
  came out as `CREATE VIEW x AS SELECT 1`.

- **`Design::parser()` and `Design::dialect()`** are public: which reader a
  project used decides what may be asked of it, and a refusal has to name the
  dialect the user configured rather than the reader it selected.

[#23]: https://github.com/sensei-hq/dbd/issues/23

### Fixed

- **`diff` and `reconcile` no longer report "in sync" for T-SQL and MySQL
  projects.** The guard added for SQLite (#20) asked `parser == Verbatim`, but
  `Verbatim` was never the only reader without a structured model: the
  statement-head readers produce identity and references and no `table_def`
  either. Both were added *after* the guard and walked straight past it, so
  desired and live reduced to nothing, the comparison succeeded trivially, and
  the answer was "no drift" against a database sharing not one table with the
  design.

  The guard now asks `ParserChoice::produces_structure()` — a property of the
  reader rather than a list to keep in sync, which is exactly what went stale.
  The refusal also names `source.dialect` as written rather than the reader it
  selected, so a SQLite project is told about `sqlite` and not about
  "verbatim".

- **A URL for a database dbd has no adapter for is refused by name.**
  `connect` dispatched on `convex:` and `sqlite:` and fell through to
  **PostgreSQL for everything else**, so `mysql://…` built a `PostgresAdapter`
  and failed with `pool timed out while waiting for an open connection` — a
  PostgreSQL error naming neither MySQL nor the missing adapter. It now says:

  > no adapter for MySQL: dbd can READ MySQL DDL (source.dialect) but cannot
  > connect to one, so `apply`, `deploy`, `diff` and `reconcile` are not
  > available. `parse_sql_as`, `project::survey` and `dbd inspect` work offline.

- **A dialect with no `search_path` is no longer warned about one.** The
  missing-`SET search_path` report added in 0.18.0 fired on T-SQL and MySQL
  projects, telling their authors that unqualified names "resolved against
  PostgreSQL's session default". Neither dialect has a search path.

### Added

- **`dbd`'s connecting commands are covered against a real database**
  (`tests/cli_live.rs`, #11). `dbd-core`'s embedded suite covers the library;
  the CLI's own layer — the `run` arms that build an adapter, and the exit code
  `main` turns a failure into — sat near zero.

  Driven as the binary rather than by adding a `[lib]` to `dbd-cli`: that
  would publish the command handlers as public API to be maintained for
  testing's sake, and driving the binary also covers argument parsing and is
  the only way to assert an **exit code**, which is what a pipeline keys on.
  Coverage reaches it — `cargo llvm-cov` instruments the binary and the spawned
  process writes its own profile, verified before the suite was written.

  `commands/mod.rs` 4.5% → 30.1% regions, `commands/reverse.rs` → 43.0%,
  `commands/migration.rs` → 14.8%.

## [0.19.0] — 2026-09-26

`Entity` carried four fields for one idea. `refers` was every name in
`references`, rebuilt by hand at each producer. `references` knew where a
schema came from but not whether the entity read or wrote it. `reads` and
`writes` knew the opposite. Asking "which of my reads has a guessed schema?"
meant joining two lists on a key that is not unique — a routine that both
reads and writes one table produced two identical rows and a duplicated name.

They are now one list. `Ref { name, kind, schema_source, unresolved }` puts
the three facts on the same row, and `RefKind` retires a `ref_type` that
distinguished only "is this a call".

Getting there meant untangling an overload first: `writes` held table names
when a parser filled it and the entity's own DDL body when the introspector
did. Nothing mixed the two in practice, but one field meaning two things is a
trap, so the body moved to `Entity::body`.

Measured against the same 2,154-file T-SQL corpus before and after:
**identical** — 2,737 entities, 8,558 reads, 1,828 writes, 1,216 calls, 123
catalogs, 14,754 total references. The shape changed; the meaning did not.

**Breaking:** any code reading `entity.refers`, `entity.references`,
`entity.reads` or `entity.writes`, and `REF_TYPE_FUNCTION` is gone. See the
migration note at the end of this entry.

### Changed

- **One list of references, replacing four parallel fields** ([#22] follow-up).
  `Entity::refs: Vec<Ref>` supersedes `refers`, `references`, `reads` and
  `writes`, with `Ref { name, kind, schema_source, unresolved }`.

  The four held overlapping views of the same facts and none held all of them.
  `refers` was every name in `references`, rebuilt by hand at each producer.
  `references` knew provenance but not direction — `ref_type` was `None` for
  reads *and* writes, `Some("function")` for calls, and `Some("table")` at two
  sites nothing ever read. `reads`/`writes` knew direction but not provenance.
  So "which of my reads has a guessed schema?" needed a join on a key that was
  not unique: a routine both reading and writing one table produced
  `refers: ["app.audit", "app.audit"]` and two identical `references` rows.

  `RefKind::{Reads, Writes, Calls, Member}` replaces the stringly `ref_type`,
  and `REF_TYPE_FUNCTION` is gone with it. Role membership is its own kind
  rather than a read, so a caller walking data flow does not find roles in it.

  `reads()`, `writes()`, `calls()`, `refs_of(kind)` and `refers()` are views
  over the list. **`refers()` omits unresolved references and the others do
  not**, which reproduces exactly what the old pair did: `refers` held only
  what resolved, so the topological sort never waited on something absent,
  while `reads`/`writes` kept the file's own account, which is what the import
  plan matches a staging table against. That disagreement used to be
  accidental; it is now one field (`Ref::unresolved`) and documented.

- **`Entity::body` — the DDL body is no longer a "write"**. `writes` meant two
  unrelated things depending on who filled it: the parser put table names
  there, the introspector put the entity's own DDL body there (a view's
  `SELECT`, a sequence's `CREATE`, one string per routine overload), and
  `emit_view`/`emit_sequence`/`emit_routine` read `writes[0]` back out as that
  body.

  No live path mixed them — `emit_entity` is reached only from `reverse`
  (introspected entities) and `matview_create_sql` (matviews, whose parser
  deliberately followed the introspector's convention) — so this was a trap
  rather than a bug. The collapse above could not happen until it was
  untangled.

- **The resolver asks provenance, not a proxy, for `refers` too.**
  `recover_bare_target_by_proxy` is gone. Every reference now carries its own
  `schema_source`, so the value-based guess ("the schema equals
  `default_schema`, so the parser must have supplied it") is no longer needed
  anywhere — closing the last of the gap #22 opened.

**Breaking:** any code reading `entity.refers`, `entity.references`,
`entity.reads` or `entity.writes`. `entity.refers` → `entity.refers()`;
`entity.reads` → `entity.reads().map(|r| &r.name)`; a view or routine body is
`entity.body`.

## [0.18.0] — 2026-09-25

An unqualified name in a SQL file means nothing without knowing where it
resolves, and dbd was answering that question with a constant. Every entity
now carries the namespace context its file established — and says who
established it.

Three things were wrong, each verified against a live PostgreSQL rather than
assumed. `"$user"` was read as a schema name, so a file writing Postgres's own
default produced references into a schema that cannot exist. A file stating
nothing was indistinguishable from one stating `public`, though the real
session default is `"$user", public` and depends on the connecting role.
And `USE db` — the T-SQL and MySQL equivalent — was ignored outright, so the
same table in two databases collapsed into one entity.

The fallback is now the project's to choose (`source.search_path`) instead of
a `public` compiled into dbd, and **it is never silent**: every DDL file that
states no path is named, with what its names were resolved against instead.

**Breaking:** `Entity::search_paths` and `ParsedFile::search_paths` are gone,
replaced by `schema_path: SchemaPath`. Code reading `entity.search_paths` as a
`Vec<String>` becomes `entity.schema_path.schemas()`.

### Added

- **Every entity carries the namespace context its file established.**
  `Entity::schema_path` and `ParsedFile::schema_path` replace
  `search_paths: Vec<String>` with a `SchemaPath` that says *where*
  unqualified names resolve **and whether the file actually said so**.

  An unqualified name means nothing without that context, and each dialect
  states it differently: PostgreSQL `SET search_path TO a, b`, T-SQL and MySQL
  `USE db`.

- **`USE db` is read, and sets the catalog.** Previously ignored outright. Over
  a 2,154-file T-SQL corpus, 77 files carry one; the reader now recognises 63
  of them, taking entities with a catalog from **2 to 123**. Without it,
  `dbo.Issues` in two databases was a single entity — the collision
  `Entity::catalog` exists to prevent. A three-part name states its own
  database and still wins.

- **`source.search_path` — the fallback path is the project's to choose**, not
  a constant compiled into dbd. Every dbd DDL file is expected to open with
  `SET search_path TO <schema>;`; nothing generates that line, so a file can
  forget it, and then its unqualified names resolved against `public` — for a
  project whose schemas are `app` and `shared`, simply the wrong answer.

  ```yaml
  source:
    search_path: [app, shared]
  ```

  A file stating its own path still wins; this only fills the gap. Omitted,
  PostgreSQL's session default stands. An empty list is refused rather than
  read as "no schemas".

  **And it is never silent.** Loading a project names every DDL file that
  stated no path, and what its names were resolved against instead: `inspect`
  counts them, `apply` and `deploy` print them. Schemaless types (roles,
  extensions) are exempt — they have no unqualified names to resolve.

  `PathSource::{File, Project, SessionDefault}` replaces `SchemaPath::stated`
  as a bool, since there are now three possible authors of the answer.

### Fixed

- **`"$user"` is no longer treated as a schema name.** A file writing
  Postgres's own default (`SET search_path TO "$user", public`) produced
  references to `"$user".lookup` — a schema that cannot exist, so an edge that
  could never resolve. It is now `PathEntry::CurrentUser`: kept on the path,
  in position, for a caller that has a connection, and never used to qualify.

- **A file that states no search_path is distinguishable from one stating
  `public`.** Both used to produce `["public"]`. Verified against a live
  server, Postgres's actual default is `"$user", public` — with a schema named
  after the connecting role, a bare `lookup` resolves to `<role>.lookup`, and
  `ALTER ROLE`/`ALTER DATABASE … SET search_path` move it further. dbd cannot
  know at parse time and said `public` as though it could; `SchemaPath::source`
  now says whose answer it is.

## [0.17.0] — 2026-09-25

Two reports from an embedder reading SQL through `parse_sql_as`, both about
references dbd was quietly getting wrong rather than failing to find.

The first ([#21]) measured dbd extracting **half** the references a reader it
had replaced found on the same corpus. Instrumenting the walk showed dbd
calling `refer()` 43,754 times against the other reader's 43,737 — within
0.04%. Nothing was being missed; 47.8% was being **discarded** one line later,
for want of a declaration to hang it on. A data script that is nothing but
`INSERT INTO a SELECT FROM b` reported nothing at all. It now reports what it
touched: **817 of 2,154 files went from silent to saying something.**

The second ([#22]) is about honesty rather than volume. A bare `REFERENCES
parent` is qualified with `search_path[0]`, and the result was the same string
as a source that wrote it — so a consumer could not tell a fact from a guess.
Now it can. Implementing it exposed a defect nobody had reported: the resolver
used the *value* as a proxy for "dbd guessed this", and could therefore
re-point a schema the source had explicitly written.

**Breaking:** `ParsedFile`, `Reference` and `ForeignKey` each gained a field.
Code that constructs them with a struct literal needs `..Default::default()` or
the new field. Nothing changes for code that only reads them.

### Added

- **A file's references are no longer thrown away** ([#21]). `ParsedFile` grows
  a `references` field — `reads`, `writes`, `calls` — carrying what the file
  referred to outside any declaration it makes.

  The statement-head walk attributed every reference to the most recent
  declaration in its batch and **dropped** anything made before there was one.
  The reasoning was half right: attaching a reference to whatever happens to be
  declared next *would* fabricate an edge. But "it belongs to the file" is a
  third answer, and dbd had nowhere to put it.

  Measured over a 2,154-file T-SQL corpus: the walk calls `refer()` **43,754**
  times and was discarding **20,929 of them (47.8%)**. An independent reader
  over the same corpus found 43,737 references — within 0.04% — so nothing was
  being missed in extraction; it was being dropped at the last step. Two-thirds
  of the loss was 278 pure data scripts, where the references are the whole
  content of the file.

  Deduplicated per file, as entity references already were, that is **4,059
  file-level references**, total 10,695 → 14,754 (+38%). The number that
  matters: **817 of 2,154 files (38%) went from reporting nothing at all to
  reporting something.**

  Nothing is attached to an entity that did not make it. Filled in by the
  `TSql` and `MySql` readers; the PostgreSQL reader leaves it empty, and that is
  not an omission — libpg_query hands back a statement list where a function
  carries its body as one node, so a reference cannot float outside its
  declaration there.

  One hypothesis was measured and rejected rather than built: re-attaching a
  later `ALTER TABLE x` batch to an `x` declared earlier in the same file is
  worth 182 references of the 20,929.

- **A reference says whether its schema was written or guessed** ([#22]).
  `Reference::schema_source` and `ForeignKey::ref_schema_source`, carrying
  `SchemaSource::{Stated, Inferred, Resolved}` (`is_guess()` for the usual
  question).

  The PostgreSQL reader qualifies a bare `REFERENCES parent` with the first
  entry on the entity's `search_path`. The result — `app.parent` — is the same
  string a source that wrote `app.parent` produces, and nothing recorded which
  it was. `resolve_references` corrects a bad guess, but it needs every entity
  in the scan, so a consumer reading one file at a time cannot run it and had
  no way to tell a confident edge from an invented one.

  T-SQL and MySQL never infer — an unqualified name is reported unqualified —
  so everything those readers produce is `Stated`.

- **The resolver no longer re-points a schema the source wrote** ([#22]).
  `recover_bare_target` used *"the schema equals `default_schema`"* as a proxy
  for *"the parser guessed this"* — its own comment called it "the parser's
  bare-qualification marker". A table that deliberately writes `app.parent`
  while its own `search_path` is `app` satisfies that test, so its explicit
  qualification could be silently re-pointed at another schema on the path that
  happened to hold a table of the same name. It now asks the recorded fact.

  Found while implementing the above, not reported.

### Changed

- **`FileKind::Empty` documents what it does and does not mean.** It means
  "declares nothing, changes nothing, moves no rows" — not "says nothing". A
  read-only script lands there and now reports what it reads (140 references
  across 109 such files in the corpus). The variant is not renamed: `"empty"`
  is the serialized value callers match on.

[#21]: https://github.com/sensei-hq/dbd/issues/21
[#22]: https://github.com/sensei-hq/dbd/issues/22

## [0.16.0] — 2026-09-25

**MySQL** joins PostgreSQL, T-SQL and SQLite: `source.dialect: mysql` selects
the same statement-head walk T-SQL uses, under rules that differ where the two
dialects genuinely disagree. It is fixture-verified rather than
corpus-measured, and says so.

The rest of this release is about the documentation, which had been drifting
for want of anything that reads it. `Design::apply`'s example was wrong on six
surfaces at once — every one of them showing seven arguments to a method that
takes five — and nothing noticed, because nothing compiled them. Now three
gates do: every Rust example in the embedder-facing docs is compiled as a test
target, the facts the guides state are checked against the code that decides
them, and broken doc links are `deny`-ed at the crate root. All three found
real defects on their first run.

The design document got the same treatment by hand. It had become a second,
wrong copy of the source — 16 of 40 field declarations inaccurate, three types
gone, and a dependency listing with no `pg_query` in it.

### Fixed

- **Doc examples that did not compile.** Found by the gate below on its first
  run, which is the point of it:
  - `Progress` and `ApplyComplete` were used without being imported, on four
    surfaces. A reader copying any of them got an unresolved-name error.
  - `design.report()` takes `&mut self`, and both `SKILL.md` copies wrote
    `let design` — while `llms-full.txt` correctly wrote `let mut design`. Two
    surfaces documenting the same call, disagreeing.

### Added

- **Every Rust example in the embedder-facing docs is now compiled**
  (`tests/doc_examples.rs`). Extracted from README, both `SKILL.md` copies and
  `llms-full.txt` into a committed file that cargo builds as a test target, so
  an example that does not typecheck is a build failure. A digest of the
  extracted blocks is embedded, and a second test fails if a doc changed
  without regenerating — compiling a stale copy would prove nothing about what
  users read.

  This exists because `Design::apply`'s examples were wrong on **six** surfaces
  at once and nothing noticed for want of anything compiling them. Verified by
  reverting one example to the old 7-argument form: the build fails with the
  original error, `this method takes 5 arguments but 7 arguments were supplied`.

  Scope is deliberate — `architecture.md`'s 34 blocks are design prose, not
  code to copy. A block opts out with ` ```rust,ignore `.

- **Broken documentation links are now an error.** Twenty-four had accumulated
  — links to items since made private, links to items that no longer exist,
  `<type>` read as an HTML tag, bare URLs. Each reads correctly in the source;
  only rustdoc knows it does not resolve.

  All twenty-four fixed, and the lints (`broken_intra_doc_links`,
  `private_intra_doc_links`, `invalid_html_tags`, `bare_urls`) are now
  `deny`-ed at both crate roots. A `deny` in the source rather than a flag in
  CI, so it travels with the crate: a contributor running `cargo doc` locally
  gets the same failure the pipeline does.

  `cargo doc` runs once per push in CI, and before publish in the release
  workflow — docs.rs builds after publish, and a publish cannot be undone.
  Deliberately **not** in the inner loop: a doc build is slow, and a broken
  link is not worth blocking a commit on.

  One scoped exemption, in `src/cli.rs`, with the reason on it: every doc
  comment there is a clap help string, so `dbd --help` is its first reader.
  `REFRESH MATERIALIZED VIEW [CONCURRENTLY]` is how Postgres writes an optional
  keyword and `<dir>/<name>.<fmt>` is how a CLI shows a path template —
  satisfying rustdoc would put backticks in what users see.

- **Facts the docs state are checked against the code**
  (`tests/docs_match_code.rs`): the two `SKILL.md` copies are byte-identical,
  every `source.parser` value the guide lists is one the resolver accepts, the
  guide's dialect→reader table matches `for_dialect_typed`, the readers the
  guide says cannot be diffed are the ones that produce no `table_def`, and
  every scaffolded `ddl/` folder is named somewhere a reader will look.

  Not wording — facts with one right answer. A test that pins a sentence breaks
  on a harmless rewrite and teaches people to delete tests.

- **MySQL is read.** `ParserChoice::MySql`, selected by `source.dialect: mysql`
  (or `mariadb`) and by `Dialect::detect`. The same statement-head walk as
  T-SQL under different rules — the walk is what every SQL dialect has in
  common; what differs is small, specific, and wrong the other way round:

  | | MySQL | T-SQL |
  |---|---|---|
  | `ALTER PROCEDURE` | refers — changes characteristics only | declares — carries the body |
  | `a.b` | `database.object` (no schemas) | `schema.object` |
  | quoting | `` `name` `` | `[name]` |
  | `#` | line comment | starts a temp-table name |

  `a.b` landing in `Entity::catalog` rather than `schema` is what keeps two
  databases' `users` tables from merging into one entity.

  **Fixture-verified only.** The T-SQL reader was measured against 2,154 real
  files; no MySQL corpus was available, so this is tested against cases its
  author thought of rather than against a codebase. The `#[ignore]`d corpus
  gate will measure it when one turns up.

- **`lex::LexRules`** — the lexer is no longer dialect-blind, because two
  dialects disagree about the same character. `#` starts a line comment in
  MySQL and a temp-table name in T-SQL: read one way in the other's file and
  either every comment becomes a phantom table, or every temp table swallows
  the rest of its line.

### Changed

- **`docs/design/architecture.md` no longer describes types that do not
  exist.** It had drifted into a second, wrong copy of the source: of 40 field
  declarations it listed, 16 were inaccurate, three types were gone entirely,
  and the dependency section reproduced all three manifests — claiming
  workspace version `0.1.0` against a released 0.15.0, a `dbd-core`
  requirement of `0.12.2`, features (`supabase`, `convex`) that were never
  built and a `rusqlite` dependency never taken, with **no `pg_query` in the
  listing at all** — the crate that reads every line of DDL dbd parses.

  Every type listing is now prose about what the type is *for*, every manifest
  is a link, and what remains is rationale a manifest cannot carry. Four
  copyable examples joined the compile gate above; the 21 illustrative ones are
  fenced ` ```rust,ignore `. The 18 end-to-end scenarios are now Gherkin — they
  are requirements, and a requirement written as Rust rots when the API moves,
  which is exactly what happened to everything else on this list.

  Net 693 lines deleted against 405 added. No behaviour changed; this is the
  document catching up with eleven releases of code.

## [0.15.0] — 2026-09-25

dbd reads more than PostgreSQL. **T-SQL** is read by a statement-head lexer —
2,737 entities and 11,602 edges from a corpus where libpg_query managed 13
declarations and a 94.5% parse-error rate. **SQLite** round-trips: a project
`init --from-db` exported could not be read back at all, because dbd wrote no
`source:` block and then rejected its own `AUTOINCREMENT`. And **16.2% of a
real SQL Server corpus was invisible** to `std::fs::read_to_string`, which
failed a whole project load rather than one file.

For callers outside dbd: `project::survey` answers "is this a dbd project, and
what is in it" without parsing anything, and `parse_sql` reads entities out of
SQL that is not in dbd's layout at all.

Two things the measurements changed. `reconcile` and `diff` reported **"in
sync" against an empty database** on any project without a structured model —
they now refuse and say why. And every documented `Design::apply` example was
uncompilable; there were no doctests on `Design` at all, which is why nothing
caught it.

Breaking for embedders: new `EntityType` and `ParserChoice` variants, and new
fields on `Entity` and `ParsedFile`.

### Fixed

- **One UTF-16 DDL file failed the whole project load.** `Design::from_config`
  read DDL with `std::fs::read_to_string` and *propagated* the error, so a
  single UTF-16 file under `ddl/` aborted the load — not that file, the load. A
  project authored in SQL Server Management Studio could not be opened at all.

  Every path that reads user-authored SQL now decodes through `source_text`:
  the project scan, RLS policy files, lifecycle hook scripts, migration SQL and
  data SQL. Measured against the corpus, the files dbd cannot read fell from
  **391 to 14**, and the share it can classify rose from 76.2% to 91.8%.

- **A SQLite project exported by dbd could not be read back by dbd** (#20).
  `dbd init --from-db sqlite://…` writes `sqlite_master.sql` into `ddl/`
  verbatim — `AUTOINCREMENT`, `WITHOUT ROWID` and `STRICT` included — but
  `reverse::design_yaml` emitted no `source:` block, so the project loaded under
  the `postgresql` default and libpg_query rejected all three. `apply` then
  refused with *"N file(s) could not be parsed"*. The round-trip dbd advertises
  did not work at all.

  `source.dialect: sqlite` is now written by `init --from-db`, and it selects a
  **verbatim** reader: the DDL file is kept as-is in `Entity::raw_ddl` and
  applied unchanged. That is not a weaker fallback — it is the shape SQLite
  already has on the other side, where `SqliteAdapter::introspect` builds each
  entity from `sqlite_master.sql` with no `table_def` because that text *is* the
  schema. Reading the files any other way made the two sides disagree about what
  a table is.

  Verified end-to-end against a real in-memory database: export a schema,
  re-apply it to an empty database, and compare the definitions both sides
  report — not just the names, since an empty table matches on names alone.

- **`reconcile` and `diff` reported "in sync" for a SQLite project** — against a
  database sharing not one table with the design. A verbatim entity has no
  `table_def`, and both snapshot builders keep only entities that have one, so
  desired and live each reduced to nothing and the comparison succeeded
  trivially. Observed: `added=0 altered=0 dropped=0` against a completely empty
  database.

  Both now refuse, naming the reason, as they already did for batch adapters.
  "In sync" is the one answer that must never be wrong. `apply`, `deploy`,
  `import` and `export` are unaffected.

- **Every documented `Design::apply` / `import_data` example was uncompilable.**
  They showed the three progress callbacks as three separate arguments; both
  methods take five, with the callbacks travelling together in one `Progress`.
  A 7-argument call does not compile.

  The root cause was `apply`'s own doc comment — *"Use `|_| {}` / `|_, _| {}` /
  `|_| {}` when progress reporting is not needed"* — and six downstream surfaces
  had copied the misreading: `README.md`, both `SKILL.md` copies,
  `docs/design/architecture.md` (twice), `docs/llms/llms-full.txt`, the live
  site, and the design mockups.

  All corrected, and `Design::apply` now carries a **doctest**, so `cargo test
  --doc` compiles the canonical example on every run. There were no doctests on
  `Design` at all, which is why CI never caught this. Verified by mutation:
  changing the doctest back to the 7-argument form fails with *"this method
  takes 5 arguments but 7 arguments were supplied"*.

### Added

- **`project::survey` — is this a dbd project, and what is in it?** The cheap
  counterpart to `Design::from_config`: reads the config and walks the layout,
  parses no SQL. For a caller walking a repository, that ordering matters —
  decide *whether* to parse a directory, and with which parser, before paying
  to parse anything.

  ```rust
  if let Some(s) = dbd_core::project::survey(Path::new("."))? {
      for file in &s.ddl_files {
          let sql = std::fs::read_to_string(file)?;
          let entity = dbd_core::parser::parse_entity_with(s.parser, file, &sql)?;
      }
  }
  ```

  Takes a project directory **or** a config path (`dbd -c` accepts a config
  under any name, so recognising only `design.yaml` would disagree with the
  CLI). Reports the project name, version, dialect, the `ParserChoice` that
  dialect resolves to, schemas, and three separate file lists — `ddl_files`,
  `policy_files`, `import_files`. Policies are SQL but not entity definitions,
  so folding them into the DDL list would invent entities.

  "Not a dbd project" is `Ok(None)`, not an error — a scanner meets far more
  non-projects than projects. A `design.yaml` that cannot be read is `Err`, and
  the distinction is deliberate: collapsing them means a malformed project is
  silently skipped as "not dbd".

  **What was excluded is reported, with a reason.** `migrations/` and
  `snapshots/` hold generated SQL — a scanner that indexed them would report
  every historical version of a table as a live entity — and
  `ddl/procedure/staging/import_jsonb_to_table.ddl` is dbd's own plumbing.
  Nothing absent is reported: a project with no generated output has an empty
  exclusion list.

- **`project::survey_json`** — the same as JSON, with `managed` as an explicit
  field rather than "object vs null", and a `reason` when the answer is no.
  `parser` is spelled as `source.parser` accepts it, so the value round-trips
  back into a config.

- **T-SQL is read.** `ParserChoice::TSql`, selected by `source.dialect: tsql`
  (or `mssql`/`sqlserver`) and by `Dialect::detect`. A statement-head walk over
  the token stream: `CREATE PROCEDURE [dbo].[sp_X]` declares; `FROM
  [dbo].[Issues]` refers.

  Measured over 2,154 real T-SQL files, against libpg_query's 13 declarations
  and 94.5% parse-error rate on the same input:

  | | |
  |---|---|
  | entities declared | **2,737** (1,285 procedure, 613 table, 506 view, 220 function, 113 trigger) |
  | edges | 8,558 reads, 1,828 writes, 1,216 calls |
  | files classified | 100% — a lexer has nothing to reject |

  Three dialect-specific rules do the work. **`ALTER PROCEDURE` declares** —
  T-SQL requires it to carry the complete body, so it replaces rather than
  edits (271 files ship procedures that way); `ALTER TABLE` never does.
  **A qualified call is an edge and a bare one is a built-in**, because T-SQL
  *requires* a scalar UDF to be schema-qualified — the distinction is read off
  the grammar rather than a list of built-in names that would go stale. And a
  `DROP x` naming something the same file declares is the **redeploy idiom**,
  not a change: counting it as one put 54.5% of the corpus in `Mixed`, and
  resolving it correctly moved 558 files to `Declaration`.

  A T-SQL entity carries **no `table_def`** — this reads statement heads, not
  column lists — so `diff` and `reconcile` cannot run on T-SQL, the same
  position SQLite is in and for the same reason.

- **`EntityType::Trigger`** — 107 trigger files in the measured corpus, so
  reporting one as a `Function` would be a visible lie. `CREATE TYPE` and
  `CREATE SYNONYM` are deliberately *not* modelled: 3 files each.

- **`parser::lex` — a SQL tokeniser.** Batches, comments, quoting; nothing
  else. The first half of reading T-SQL, and a lexer rather than a grammar
  because dbd measured the alternatives on a 2,154-file corpus and none of them
  can read the statements it wants. After splitting `GO` batches — the most
  generous way to ask — `sqlparser`'s `MsSqlDialect` loses **99% of
  `CREATE PROCEDURE`, 100% of `ALTER PROCEDURE`, 95% of `CREATE TABLE`**. It
  passes `SET`, `IF EXISTS` and `INSERT`, so an 81.9% batch-level pass rate
  hides a near-total loss of exactly the facts a reader is reading for.
  Microsoft's ScriptDom is complete and is .NET, a runtime dependency dbd does
  not have.

  Over the same corpus the lexer reaches **99.57% of batches** (83 of 19,299
  yield no tokens), producing 2.5M tokens of which 1.16M are names.

  `GO` is separated before anything reads a batch: it is a client directive,
  not SQL, so no grammar accepts it. Comments and literals are *consumed* —
  a table named in a comment is not a reference, and a real corpus is full of
  commented-out SQL. Nested block comments, `[bracket]]escapes]`, `''` in
  literals, and `@p`/`@@ROWCOUNT`/`#temp` consumed whole so their tails never
  lex as phantom tables.

- **`ParsedFile::kind` and `ParsedFile::dialect`, plus `parse_sql_as`.** A SQL
  codebase is mostly not declarations — sensei measured `ALTER TABLE`
  outnumbering `CREATE TABLE` 159 to 101 — and a change script that minted an
  identity for the table it alters would give a caller two nodes for one table.
  `FileKind` tells "owns this entity" from "touches it":

  | | |
  |---|---|
  | `Declaration` | declares entities, changes nothing it does not declare |
  | `Migration` | `ALTER`/`DROP` on objects defined elsewhere — edges, not nodes |
  | `Data` | `INSERT`/`UPDATE`/`DELETE`/`MERGE`/`COPY` |
  | `Mixed` | declares *and* changes something else |
  | `Empty` | nothing dbd recognises |

  A declaration's **own** index and comment are part of it, not changes to
  something else — otherwise every ordinary dbd table file would land in
  `Mixed`.

  `parse_sql_as(dialect, sql)` is the multi-dialect entry point; pair it with
  `Dialect::detect`. The result records `Unstated` when nothing identified the
  file, rather than claiming the fallback reader's dialect as the file's own.

- **`source_text` — decoding a file before any parser sees it.** `std::fs::
  read_to_string` rejects anything that is not UTF-8, and SSMS writes UTF-16LE
  by default. Measured over a real SQL Server corpus of 2,421 `.sql`/`.ddl`
  files: **377 UTF-16 with a BOM (15.6%) and 14 other non-UTF-8 (0.6%)** —
  16.2% invisible before any grammar was involved.

  A BOM is a positive statement of encoding and is read **first**, because
  UTF-16LE ASCII is `X 00 X 00` and any null-byte test would otherwise call
  every UTF-16 file binary. The BOM is then *consumed*: a parser handed
  `\u{feff}CREATE` reports a syntax error on line 1 of a valid file.

  No BOM means UTF-8 is required. Charset detection — guessing latin-1 from
  byte frequencies — is deliberately not done: `NotUtf8` is already the
  actionable answer, and guessing invents characters the source never carried.
  A lossy decode is refused for the same reason, since U+FFFD in an identifier
  is a name no use site could mint.

  Ported from sensei's `classifiers::decode_source`, which reads the same trees
  and had measured the same split.

- **`parser::Dialect` — which SQL a file is, stated or detected.** Distinct
  from `ParserChoice`, which is which reader dbd *runs*: several dialects share
  a reader, and a dialect dbd has no reader for still has a name.
  `ParserChoice::for_dialect_typed` is the single place one becomes the other,
  so a config label and a detected dialect can never select different readers
  for the same SQL.

  `Dialect::detect` **fails closed**. `CREATE TABLE t (id int)` is valid in
  every dialect and says nothing about which one it is in, so it is `Unstated`
  — not a default, and not a guess. A tie between two dialects is `Unstated`
  too. Markers are ported from sensei's SQL indexer, where they were scored
  against a real multi-dialect corpus.

  Nothing changes for existing projects: an unrecognised `source.dialect` still
  falls back to libpg_query rather than erroring.

- **`Entity::catalog` and `Entity::qualified_key()`** — the database level,
  for telling two same-named tables in different databases apart.

  `None` for PostgreSQL, always: cross-database references are impossible on
  one connection, so the name would distinguish nothing. `Some` for T-SQL and
  MySQL, where `OtherDb.dbo.Users` is an ordinary reference and MySQL's
  `db.users` puts the *database* where dbd's model expects a schema. Without
  the level, `dbo.Users` in two databases is one entity and a multi-database
  scan merges them silently.

  `resolve_references` now keys on `qualified_key()` (`catalog.schema.name`, or
  `schema.name` without one). A reference naming no catalog resolves within the
  **referring entity's** catalog first, then against a catalog-less entity —
  mirroring how a bare schema already resolves along `search_path`. One naming
  a catalog is taken at its word, and stays unresolved if that catalog is not
  in the scan rather than falling back to a local table of the same name.

  Invisible to every existing project: with no catalog anywhere the key *is*
  the name, so the resolution set is byte-identical. `catalog` is
  `skip_serializing_if = "Option::is_none"`, so snapshots neither churn nor
  need migrating.

- **`config::ProjectConfig::version()` and `DEFAULT_PROJECT_VERSION`** — one
  answer to "what version is this project" when `design.yaml` omits it: **1**.
  `dbd release` used `unwrap_or(1)`, so that behaviour is unchanged; the value
  now has a name and a home.

  `dbd merge`'s version-safety gate deliberately keeps its own floor of **0**
  and is unchanged. It is not asking what version the project is — it is
  choosing how permissive to be, and a project declaring no version has made no
  claim to be ahead of any database. Flooring it at 1 would refuse an ordinary
  first merge, since a managed database with no row for this project reports 0.
  The difference is now documented on both sides and pinned by a test, so it
  cannot be "tidied up" into a bug.

- **`source.parser: verbatim`** — selects the verbatim reader explicitly.
  `dialect: sqlite` implies it; the override exists for anything else whose DDL
  should be applied as written.

## [0.14.0] — 2026-09-24

One parser. The sqlparser DDL path retires — it was a second *PostgreSQL*
parser, not a dialect, kept as an escape hatch during the libpg_query migration
and unreachable since every entity type went native in 0.13.0. Removing it takes
the last regex out of the DDL parse path with it.

In its place, two things the migration made possible. `parse_sql` gives an
external embedder the statement-level identity the path-derived `parse_entity`
could not (issue #19), and `ALTER TABLE … ADD CONSTRAINT` is finally read —
until now a constraint written that way was silently dropped from the model, on
the live apply path.

Breaking for embedders and for any project that names `source.parser:
sqlparser`; see **Changed** below.

### Added

- **`parser::parse_sql` — statement-level parsing for external embedders**
  (issue #19). Reads every entity a SQL file declares, taking type, schema and
  name from the **statements** rather than from the path.

  `parse_entity` derives identity from `ddl/<type>/<schema>/<name>.ddl`, which
  is correct inside dbd's layout and silently wrong outside it: it falls back to
  `EntityType::Table` and names the entity after a directory, so a stored
  procedure reads as `Table Users.sp_NewMCRIssue` with only `entity.errors` to
  hint otherwise. `parse_sql` asks the SQL instead.

  ```rust
  let parsed = dbd_core::parser::parse_sql(sql)?;
  for entity in &parsed.entities {
      // entity.entity_type, entity.schema, entity.name (qualified),
      // entity.refers / references (typed edges),
      // entity.reads / writes (separated, for routines)
  }
  ```

  Returns `ParsedFile { entities, search_paths, errors }`, holding dbd's own
  `Entity` — the read/write split and the soft/hard reference distinction are
  the parts an embedder cannot get elsewhere, so nothing is flattened. Several
  declarations in one file become several entities; `CREATE INDEX` and
  `COMMENT ON` fold into the entity they *name*, not the nearest preceding one.
  Reference resolution stays in `references::resolve_references`, so the scan
  itself touches no shared state and parallelises.

  `parse_sql_with(ParserChoice, sql)` takes an explicit parser; pair it with
  `ParserChoice::resolve` to derive one from a dialect string.

### Removed

- **The sqlparser DDL path is gone** — `extractors.rs`, `tables.rs`, the
  `preprocess_sql` workarounds, the parser parity gate, and the
  `ParserChoice::Sqlparser` variant. 1,861 lines.

  It was a second *PostgreSQL* parser, not a dialect: `parse_with_sqlparser`
  hardcoded `PostgreSqlDialect` for its entire life. It existed as an escape
  hatch during the libpg_query migration, and the migration's own design note
  named its retirement condition — "once Table is native, `SqlparserDdl` has no
  production callers". Every file-backed entity type has been native since
  0.13.0, so nothing reached it: `PgQueryDdl::native` returns `Some` for all
  eight types, and `EntityType::from_folder_name` cannot produce the four it
  doesn't cover.

  This also removes the last regex in the DDL parse path —
  `extract_proc_reads_writes`, whose own doc comment recorded that it "can
  over-match … and is blind to read/write classification". The libpg_query
  parsers have none.

  `sqlparser-rs` is still a dependency: `dbd format` and enum-candidate
  detection use it. It no longer reads DDL.

### Fixed

- **`ALTER TABLE … ADD CONSTRAINT` was silently dropped from table DDL.** A
  constraint is legitimately written inline *or* as a trailing `ALTER TABLE …
  ADD CONSTRAINT`, and the second form was read by nothing — no parser matched
  `AlterTableStmt`. On a file adding an FK, a UNIQUE and a CHECK that way:

  ```
  errors      : []      ← no error, so `ensure_fully_parsed` did not refuse
  refers      : []      ← the FK was not a dependency edge
  constraints : 0       ← all three gone
  ```

  Three consequences, worst first. The missing `refers` edge let
  `sort_by_dependencies` order a child before its parent, so a fresh `apply`
  could fail. `apply` created the table without the constraints. And
  `reconcile` saw an FK live-but-not-desired and planned
  `DROP CONSTRAINT <live-name>` — gated behind `--allow-destructive`, but a
  project that passes that flag would have lost it.

  dbd never emits this form itself, which is why it survived: it only affected
  a hand-authored file.

  Constraints added this way now go through the same `extract_table_constraint`
  as inline ones — same `TableDef`, same `PRIMARY KEY` column marking, same FK
  edge — so the two spellings are indistinguishable downstream. An `ALTER`
  naming a table the file does not declare is not absorbed.

  Other `ALTER` subcommands remain out of scope: a dbd table file is the full
  and final definition, and `ADD COLUMN` / `ALTER COLUMN` belong to generated
  migrations, which `scanner::scan_ddl` never reads. They now raise a **warning**
  on the entity rather than vanishing — silence is what kept the missing
  constraints invisible.

- **The pre-commit hook gated only the CLI.** `.githooks/pre-commit` ran
  `cargo test` and `cargo clippy` without `--workspace`. The root package is
  `dbd-cli`, deliberately not a workspace member, so both compiled the CLI and
  stopped — `dbd-core` (parser, differ, adapters, 977 tests) was never built.
  The hook printed "All checks passed." over a tree `cargo test --workspace`
  failed with exit 101.

  It now delegates to `make _check-ci`, the same pre-flight `make bump` runs,
  which has had `--workspace` all along. The two were hand-maintained copies of
  one list and drifted; there is one definition of green now, pinned by
  `tests/pre_commit_hook.rs`. Contributor-facing only — CI and `make bump` both
  pass `--workspace`, so no release shipped behind it.

### Changed

- **BREAKING — `source.parser: sqlparser` is rejected.** A project still naming
  it fails to load with a message saying it was removed and naming `pg_query`.
  Silently switching a project to a parser its author did not choose is the
  failure mode `ParserChoice::resolve` already refused for a typo; a retired
  value is held to the same bar.

- **BREAKING — `parser::extract_search_paths` is no longer exported.** It was
  the one public item that leaked `sqlparser::ast::Statement` into `dbd-core`'s
  API. Embedders wanting search paths get them from `Entity::search_paths`.

- **A non-PostgreSQL `source.dialect` now resolves to the PostgreSQL parser**
  rather than to sqlparser. For every project dbd generates this is a no-op —
  `reverse::design_yaml` writes no `source:` block at all, so a project built by
  `dbd init --from-db sqlite://` has always loaded under the `postgresql`
  default. A **hand-written** `dialect: sqlite` previously got sqlparser, which
  reads some SQLite DDL; it now gets libpg_query, which rejects `AUTOINCREMENT`,
  `WITHOUT ROWID` and `STRICT`, so such a project will be refused by
  `ensure_fully_parsed` instead of partially applied. SQLite DDL is not a
  Postgres subset and never parsed correctly here; giving it a real grammar is
  tracked separately.

- **`make install` now reclaims `target/`, matching `make bump`.** `cargo install
  --path .` builds into `target/`, so the bare install left behind roughly a
  gigabyte it had just created — and running it after a release silently undid
  the reclaim the release had performed. Both entry points now share one
  `INSTALL_AND_RECLAIM` block and end in the same state.

  Shared as a plain make variable rather than a recursive `$(MAKE) install`,
  deliberately: make executes any recipe line containing `$(MAKE)` even under
  `-n`, so a recursive call would turn `make -n bump` into a real wipe. Two
  tests pin that by side effect, because the printed recipe looks identical
  either way.

### Security

- **`rustls` 0.23.44 → 0.23.45** — RUSTSEC-2026-0285, "TLS 1.3 handshake messages
  incorrectly accepted across encryption level boundaries" (medium, 5.3). The
  advisory was published 2026-09-14 and the pin predates 0.13.0, so `cargo audit`
  went red on the first push to `main` after it landed rather than on any change
  of ours. Lockfile only — `rustls` reaches the tree transitively through
  `reqwest`/`tokio-rustls`, and nothing in this repo declares it directly.

## [0.13.1] — 2026-09-17

Two reconcile convergence fixes in `dbd-core` (#16, #17), released alongside the
docs-site, CI and repo-metadata work that had been accumulating unversioned. That
work alone left the Rust tree byte-identical to 0.13.0 and so carried no bump;
these two fixes change `dbd-core`, which is what makes this a patch release.

### Security

- **`devalue` 5.8.1 → 5.9.2 in the docs site** — "reject out-of-bounds indices"
  (AIKIDO-2026-869882). The fix was already inside `@sveltejs/kit`'s declared
  `^5.8.1`; only the lockfile was stale. `devalue` is an external import in the
  deployed worker (`output/server/index.js`), so this reaches production, not
  just the build. Exposure was low regardless — every route is prerendered, so
  it only ever parsed build-time-static payloads.

- **`undici` override 7.29.0 → 8.10.2.** The old pin dragged `jsdom@30` off its
  own declared `^8.9.0` and onto the 7 line, and `miniflare` pins 7.29.0
  exactly, which is vulnerable to the 2026-09-04 advisory batch (patched at
  7.29.1 and 8.10.2). Bun does not support nested overrides — it warns and
  ignores them — so one version serves both parents; 8.10.2 is the only fully
  patched release that also satisfies jsdom. Crossing miniflare's major is
  contained: nothing in this repo invokes miniflare, and the deployed worker
  runs on workerd.

- Secret scanning and push protection enabled on the repository. Renovate now
  owns security PRs (`vulnerabilityAlerts` + `osvVulnerabilityAlerts`, the
  latter being what actually covers crates.io) rather than adding Dependabot
  security updates as a second bot on the same job. Every Renovate rule gained a
  7-day `minimumReleaseAge` — the `@rokkit/*` group auto-merges, and automerge
  with no cooldown turns one compromised publish into a merged commit.

### Fixed

- **`reconcile` aborted on any project containing a `STORED` generated column**
  ([#16]). Postgres keeps a `GENERATED ALWAYS AS (…) STORED` expression in
  `pg_attrdef` — the same catalog an ordinary `DEFAULT` lives in — and
  introspection read it as one. Reconcile then saw a default the design never
  declared and planned `ALTER COLUMN … DROP DEFAULT`, which Postgres refuses
  outright (*"column … is a generated column"*), failing the whole run even
  when the column matched the design exactly. Introspection now reads
  `pg_attribute.attgenerated` and both parsers keep the expression on the new
  `ColumnDef::generated`, so the two sides converge. A genuinely changed
  expression now emits the verb Postgres accepts — `SET EXPRESSION AS` (PG17+),
  or `DROP EXPRESSION` when the generation is removed — and the emitter renders
  the `GENERATED … STORED` clause, which stops `reset`/`diff` from silently
  recreating a computed column as a plain one.

- **A `CHECK` or partial-index predicate over a `varchar` column never
  converged** ([#17]). Postgres rewrites such a predicate to add the implicit
  `::text` cast before storing it (`name = lower(name)` becomes
  `(name)::text = lower((name)::text)`), so the design key and the live key
  could never match: the constraint was dropped and re-added with an identical
  definition on every single run. Because those are drops, an otherwise additive
  change could not be applied without also authorising `--allow-destructive`.
  Canonicalization now erases a `::text` cast on a column Postgres
  **binary-coerces** to `text`, which is `varchar` alone — `pg_cast` records it
  as `castmethod = 'b'`, a runtime no-op. `char(n)` is deliberately excluded:
  its cast runs `rtrim1` and strips trailing spaces, so erasing it would change
  what the predicate accepts. A cast on any other column (`n::text` where `n` is
  `integer`) is the author's and still stands.

- **`bun run check` could not run at all.** `svelte-check` 4.x refuses to start
  when the `typescript` package is major 7; the site had been on `typescript@7`
  with nothing in CI to notice. `typescript` is now `~6.0.3` with TypeScript 7
  alongside as `@typescript/native` and `--tsgo` on the script, which is the
  pairing svelte-check documents. It reports 46 files, 0 errors.

- **The `cookie` override was mis-documented.** It was grouped with the
  build-time-only pins, but `cookie` is an external import in the deployed
  worker. It is pinned because `@sveltejs/kit` 2.70.3 still declares `^0.6.0`
  and every 0.6.x carries GHSA-pxg6-pf52-xh8x; 0.7.0 changed no API, so kit is
  safe on it. Renovate is now capped at `<1.0.0` for `cookie`, because 1.0
  delegates quote-parsing to `decode` (kit passes an identity decoder) and 2.0
  renames `parse`/`serialize` outright. No version changed — only the reasoning
  is now recorded and enforced.

### Added

- **CI gates the docs site** — `bun install --frozen-lockfile`, `check`, `test`,
  and a build with `CF_PAGES=1` so it exercises the adapter that actually ships.
  Nothing ran the site before, which is how the broken type-check survived. The
  frozen lockfile is the gate, not a speed-up: it fails when `bun.lock` and
  `package.json` disagree, which is exactly how `devalue` went stale.

- **CodeQL** over `rust`, `javascript-typescript` and `actions`. The `actions`
  pack is deliberate — this repo pins every action to a SHA by hand, and that
  pack checks the posture mechanically.

- **`sensei.library.json` completed against the manifest spec** ([#13]):
  `llms` (pointing at `/llms-full.txt`, the 1103-line corpus, not the 147-line
  summary), `documents`, `ref`, `ecosystem`, `packages` and `install`. The
  `install` block names `dbd install`, the command that exists — the issue's
  suggested `dbd skills add <name>` was a template from another library.
  `make bump` now rewrites `documents` and `ref`, so they cannot silently rot
  into claiming docs describe a release they do not.

## [0.13.0] — 2026-09-11

Two `dbd reconcile` non-convergence bugs ([#12]) and a security sweep.

### Breaking (embedders of `dbd-core` only — the `dbd` CLI is unaffected)

- `entity::TableConstraint::Unique` gains a `nulls_not_distinct: bool` field.
  Code that constructs the variant, or pattern-matches it exhaustively, no
  longer compiles. Add `nulls_not_distinct: false` to keep today's behaviour, or
  `..` to the pattern. Snapshots written by earlier versions still deserialize —
  the field is `#[serde(default)]`.
- `diff::generate_data_sql` now emits quoted identifiers and literals
  (`UPDATE "public"."users" SET "status" = …`). Any test pinning the previous
  unquoted output needs updating. See *Security* below for why.

### Fixed

- **`unique nulls not distinct` was silently reduced to a plain unique on the
  `ALTER` path** ([#12]). `dbd apply` on a fresh database ran the DDL verbatim
  and kept the clause; `dbd reconcile` altering an existing table re-derived the
  constraint from the parsed model, which had nowhere to hold it — so the
  constraint that landed was weaker than the one declared. Nothing reported it:
  UNIQUE constraints were compared by name alone, so `dbd diff` then called the
  result in sync. The clause now survives parsing (both the sqlparser and
  libpg_query paths), introspection, comparison and emission.

- **`dbd reconcile` would not remove an enum value, and could not converge**
  ([#12]). Postgres has no `ALTER TYPE … DROP VALUE`, so no SQL was emitted,
  reconcile reported `0 altered`, and `dbd diff` reported the same drift on
  every subsequent run. Reconcile now performs the type recreation under
  `--allow-destructive` — see *Added*.

### Added

- `dbd reconcile --allow-destructive` now converges an enum that lost a value,
  by recreating the type: dependent managed views are dropped (deepest first),
  column defaults are taken off, the type is renamed aside and recreated with
  exactly the declared values in declared order, each column is moved across
  with `USING …::text::<type>` (`::text[]` for an array column), defaults are
  restored and the displaced type dropped. Managed views are re-applied by the
  pass that already re-applies every view on every run.

  The whole batch runs in one implicit transaction, so every failure is total
  and names the object: a row still holding the removed value fails the cast, a
  default naming one fails the `SET DEFAULT`, and an unmanaged dependent view
  fails the `ALTER … TYPE`. A dependent **materialized view** is declined rather
  than attempted — dbd never auto-drops one — and reported with the manual steps.

- `dbd-core` gains `reconcile::plan_enum_recreation`, plus two modules extracted
  from previously-duplicated private helpers: `path_safe`
  (`is_safe_segment`, `safe_relative_path`) and `sql_quote`
  (`literal`, `ident`, `qualified`).

### Security

- **Database-derived names could escape the directory they were written into.**
  `dbd export` names its output file after the table and, without `--out`, the
  directory after the schema. Both come from the live catalog, and a quoted
  identifier may hold anything — `CREATE TABLE "../../x"` is legal in both
  Postgres and SQLite, and `Path::join` with an absolute name discards the base
  outright. Neither adapter checked. Both now refuse and name the object.
  `dbd reverse` and hook-path resolution already had this check, but as two
  private copies covering two of the four sites; the rule now lives in
  `path_safe` and is applied at all of them.

- **Catalog names were interpolated into SQL unescaped.** The Postgres export
  built its identifier by string-replacing the dot, leaving an embedded `"` free
  to close the quoting; the SQLite export did the same; and
  `diff::generate_data_sql` pasted table, column and enum label straight into a
  data-correction script — a file an operator runs by hand, usually with more
  privilege than dbd itself holds. All now go through `sql_quote`, which mirrors
  Postgres's own `quote_literal`/`quote_ident`. Type names stay verbatim
  (`varchar(50)` is a type expression, not an identifier).

- **`dbd diagram` handed an unvalidated URL to the platform's browser opener.**
  The base comes from `--site`/`$DBD_DIAGRAM_URL`, and the opener is not uniform
  — on Windows it runs `cmd /c start`, where a metacharacter is a command, and
  every platform will act on `file://` or `javascript:`. Only plain `http(s)`
  URLs are opened now; anything else is printed with the reason it was not.

- **Dependency advisories cleared.** `cargo audit` 4 vulnerabilities + 4 warnings
  → 0 (`crossbeam-epoch`, `h2`, `quinn-proto`, `anyhow`, `event-listener`,
  `chacha20`, and `indicatif` 0.17 → 0.18 to drop the unmaintained
  `number_prefix`). `bun audit` in `site/` 21 → 0. CI now runs `cargo audit`.

  One advisory is documented rather than fixed, in `.cargo/audit.toml`:
  RUSTSEC-2023-0071 (`rsa`) has no fixed release and is reachable only through
  `sqlx-mysql`, which dbd never enables — it is in `Cargo.lock` only because
  that file is feature-agnostic, so no feature selection can remove it.

[#12]: https://github.com/sensei-hq/dbd/issues/12
[#13]: https://github.com/sensei-hq/dbd/issues/13
[#16]: https://github.com/sensei-hq/dbd/issues/16
[#17]: https://github.com/sensei-hq/dbd/issues/17
[Unreleased]: https://github.com/sensei-hq/dbd/compare/v0.23.0...main
[0.24.1]: https://github.com/sensei-hq/dbd/releases/tag/v0.24.1
[0.24.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.24.0
[0.23.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.23.0
[0.22.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.22.0
[0.21.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.21.0
[0.19.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.19.0
[0.18.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.18.0
[0.17.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.17.0
[0.16.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.16.0
[0.15.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.15.0
[0.14.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.14.0
[0.13.1]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.1
[0.13.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.0
