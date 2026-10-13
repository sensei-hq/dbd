# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html); while
the crates are `0.x`, the **minor** position is the breaking one, so
`0.12.x → 0.13.0` may require changes in code that embeds `dbd-core`.

## [Unreleased]

### Added

- **Each entity in a shared database has one owner** (#7, phase 2). A
  project's `apply`, `deploy`, `reconcile` and `reset` claim every entity its
  whole design declares — tables, views, materialized views, sequences, enums,
  functions and procedures, and the composite, domain and range types (with a
  range's multirange) a file creates beside them; the whole design, not the
  scope it runs under — in
  `dbd.objects`. A project declaring an entity another project holds is
  refused before its first hook or statement, naming each: `relation
  app.orders is owned by "shop"; owners sharing a database must not declare
  the same entity`. A dry run, `diff`, `import`, `refresh` and `policies` —
  the CLI's `--dry-run` previews included — refuse the same way and claim
  nothing.

  When a run claims depends on whether its work can roll back. An `apply` in
  one transaction claims as its last step, in that transaction, so the claim
  commits with the work or not at all, and a run that fails or is killed
  claims nothing; a version adding enum values commits part-way, and the run
  claims before that commit too. `reconcile`, `reset` and an `apply` under
  `DBD_NO_TX` commit as they go, so they claim before their first write —
  an apply's `apply.before` hooks included — and keep the claim if they then
  fail. Of two projects racing to claim one
  entity, the first owns it and the other is refused before it writes.

  An entity is owned as Postgres names it: every `CREATE` in its file names
  one — a `DO` block's included, so the quoted `"public"."Role"` dbd writes is
  `Role` — in the schema it states, else its search path, an unquoted name
  folded to lower case. One no statement names — a file dbd cannot read, a
  name built in dynamic SQL — is claimed by its path, folded, since a reset
  still drops it. Everything Postgres names as a relation or a
  type is one namespace, so a table and an enum of one name are one entity.
  Entities a project already has are claimed on its first run; what no design
  declares stays unowned. A claim waits at most 5 seconds for another
  session's write to the registry, then fails naming it. A renamed project's
  objects stay with its old name, so even a forced `reset` of the renamed
  design is refused for them.
- **A project records what it uses**: the schemas its entities live in, its
  extensions and roles, and each reference one of its entities makes to
  another project's — `relation:app.orders`, through `view billing.v_orders`
  — in `dbd.uses`, afresh by every `apply`, `deploy` and `reconcile` that
  succeeds; a run that fails keeps the uses it had. A call is a use of a routine, anything else of a relation.
  A column of another project's enum, or a default drawing on its sequence, is
  a use without being declared `external`, however `nextval`'s string spells
  it. A name written bare is looked up as Postgres looks it up: a read is of
  the first relation on the search path that exists; a call, which Postgres
  also resolves by its arguments, is a use of every routine on the path it
  could reach.
- **`reconcile --prune` prunes by ownership** (#7, phase 2). Another
  project's table is never dropped, and `reconcile` and `diff` no longer
  report it as drift. `--prune` drops a table this project owns and no longer
  declares, and one nobody owns in a schema no other project uses. One nobody
  owns in a schema another project uses — or any, while a project the
  database records has not upgraded dbd — is kept and named: `unowned:
  app.tmp_report — not dropped because app is shared with "billing"
  (--prune-unowned drops it)`. `reconcile --prune-unowned` drops those too,
  naming each. A prune claims what it drops in the transaction that drops it,
  so a table another project claims meanwhile is kept, naming it, and a run
  that fails keeps no such claim; a table the design creates under a spelling
  its path does not share is never pruned; and removing an enum
  value that a column of another project's table holds is refused, dry run
  included, naming the column and its owner. A successful `apply`, `deploy`
  or `reconcile` releases what its
  project no longer declares once it is gone — pruned, or dropped by a
  migration or by hand; until then it stays the project's. A prune's `DROP`
  now quotes the table it names: `app."Report"` written unquoted dropped the
  `app.report` beside it.
- **What another project depends on is guarded** (#7, phase 2). Before
  `reconcile` drops a column, constraint, index or enum value, or `apply` runs
  a migration that drops a table, column, constraint, index, schema or
  extension — inside a `DO` block too — dbd asks Postgres's dependency graph
  what a `CASCADE` would reach, part by part: a view, an inheriting table or
  partition, a foreign key, a column of the type or an array of it. It also
  asks the uses other projects recorded, which see into function bodies —
  recorded whoever owned the target then, a table file's trigger function's
  included. A migration dbd cannot parse is refused where another project
  shares the database; a `TRUNCATE … CASCADE` is refused only for another
  project's table referencing the truncated one. Anything another project
  depends on refuses
  the run, before any write and dry run included: `app.orders.total is used by
  "billing" (view billing.v_totals); remove that use first`. A `--prune`
  keeps such a table, naming the use, where its `CASCADE` took the other
  project's view with it.

### Library API

- `DatabaseAdapter` gains `claim_objects` and `record_uses` (defaults that
  do nothing, so an adapter that does not override them builds unchanged),
  with the `adapter::OwnedObject`, `HeldObject` and `Use` they take and
  return; and `Design::check_ownership` refuses a design declaring an entity
  another project owns.
- `Design::reconcile` takes `prune: impl Into<Prune>` — `Prune::Off`, `On`
  or `Unowned`; a `bool` still reads as `Off` or `On`. `DatabaseAdapter` gains
  `ownership` and `release_objects` (defaults: nothing owned, nothing
  released), with the `adapter::Ownership` the first returns.
- `DatabaseAdapter::dependents` (default: none) reports other owners'
  dependences on what a run drops, with `adapter::Guarded`, `GuardedPart`
  and `Dependence`. `ReconcilePlan::dropped_parts` lists what a plan drops
  from an existing table or enum, part by part (`DroppedPart`,
  `DroppedKind`).
- `Entity::refs` keeps a schema-qualified type or sequence reference that
  resolves to nothing — another project's, an extension's — marked
  `unresolved` and unwarned, where it was dropped. A bare one is still dropped
  as a built-in. A sequence a `nextval` default names is read as Postgres
  reads the string — `nextval('App.Seq')` refers to `app.seq` — and a type or
  sequence reference reaches the sequence or enum whose mixed-case path names
  it, folded (`Order_No.ddl`).

### Fixed

Projects sharing one database no longer undo each other's bookkeeping, refresh
jobs or imports — the first phase of [#7], fixes that were wrong with a single
project too.

- **A heal folds only its own project's legacy bookkeeping.** Upgrading one
  project folded every project's rows out of a legacy `_dbd_meta` and
  `_dbd_migrations` into `dbd.*` and dropped the tables — so a project still on
  an older dbd lost its record and read the database as fresh. A heal now
  moves the project's own rows; another project's stay, and so does a legacy
  table while it holds any, unaltered. A project not yet healed reads its own
  record there, so its guards still see a database it marked prod. The heal
  locks a legacy table before reading it, reading committed whatever the
  database's isolation default, so a row an older dbd commits meanwhile is
  seen, not dropped with the table; it locks them in the order an older
  dbd's batch writes them, so the two never deadlock; it waits at most 5
  seconds for another session's write, then fails naming the table; and one
  heal runs at a time, so two projects' first runs no longer collide creating
  the `dbd` schema. An emptied table is dropped, or kept if Postgres refuses
  (a view over it). A table the project's role cannot read — another
  project's — is passed over, by reads too; one it can read but not take that
  holds its own row is refused, naming the table.
- **A pg_cron refresh job is named for its project** —
  `dbd-refresh:<project>:<schema>.<name>` — and a sync unschedules only its own
  project's. It unscheduled every `dbd:refresh:` job it was not given, so each
  project's apply took the other's jobs; the new namespace is out of reach of
  a project sharing the database that has not upgraded, whose sweep still
  takes `dbd:refresh:%`. A name fits the 63 bytes pg_cron 1.3 and 1.4 keep (a
  hash ends a longer one), and a `:` or `%` in a project's name is encoded, so
  one project's name cannot reach another's jobs. A job an older dbd named
  `dbd:refresh:<schema>.<name>` is replaced for a matview the project
  declares, and unscheduled, by any project, once the view it refreshes no
  longer exists — read from the catalog, so a schema the role cannot use
  fails nothing; concurrent syncs take turns.
- **Each project stages its JSON imports in its own table**,
  `dbd."import_tmp:<project>"`. Every load dropped and recreated one
  `dbd.import_tmp`, so two projects importing at once emptied each other's
  staged rows. A name past Postgres's 63-byte limit, which it would cut short
  and so merge with another, ends in a hash of the whole name.

[#7]: https://github.com/sensei-hq/dbd/issues/7

## [0.25.1] — 2026-10-12

**Two fixes for refusals that left no way through.** `reconcile` failed on every
run after the first in a project whose extensions live in their own schema,
once a view read one of their columns; and a scoped `reset` refused any scope
whose tables carry a policy or trigger calling one of its own functions.

### Fixed

- **A type an extension installs outside the `search_path` is no longer
  drift.** With pgvector or citext in an `extensions` schema, introspection
  reads `extensions.vector(384)` where the DDL, reaching the schema through its
  own `set search_path`, wrote `vector(384)`. Compared as text they differed,
  so every `reconcile` after the first planned an `ALTER … TYPE` that changed
  nothing — and failed, "cannot alter type of a column used by a view or rule",
  once a view read the column. `dbd diff` reported the same false drift. Both
  now spell an extension's type as the database does — a modifier, an array
  and an extension's domain included, whichever path a pooled connection or
  the database (Supabase's reaches `extensions`) reads it through — and a real
  change to one emits `TYPE extensions.citext`, which resolves wherever the
  session path points. Only a name the extension alone owns is read as its
  type: a bare `date` stays `pg_catalog.date` on a database with orafce's
  `oracle.date`, and a name the project's own enum or domain, or a second
  extension, also has is compared as written, as before. ([#41])
- **A scoped reset drops its tables' policies and triggers first.** A policy
  from `policies/`, or a trigger written in its function's file, that calls a
  function the scope also drops refused the reset — "cannot drop function …
  because other objects depend on it" — and the refusal blamed a dependent
  outside the scope for one inside it. The reset drops in reverse apply order,
  where neither has a place: policies are applied after every entity, and a
  trigger in its function's file makes the function depend on the table, so
  the function went first while its table still stood. A table's policies, and
  a table's or a view's triggers, now go first, with their table — nothing
  else is taken. A foreign key's own triggers are left, and so is a partition's
  clone of its parent's trigger (a table attached with `ATTACH PARTITION`),
  which goes with the parent's; a policy or trigger on a table outside the
  scope still refuses the reset.

### Library API

- `DatabaseAdapter` gains `extension_types()`, the `(schema, name)` of each
  type the database's extensions own. It has a default (none), so an adapter
  that does not override it builds unchanged.

[#41]: https://github.com/sensei-hq/dbd/issues/41

## [0.25.0] — 2026-10-12

**Every command does what its documentation says, and every feature works the
same under every command.** Audits drove this release: the guides read
against the code, and — three times over — every feature run through every
command with and without `--scope`, against a real database. They found
behaviour the docs promised and the code never had, and features one command
handled that another dropped — and fixed both in the code, test first.

**Upgrade before your next `reconcile --prune`, `reset`, `import` or
`format`.** Several of the fixes stop data loss:

- `reconcile --prune` dropped tables in `public` that the design never
  declared, whenever the design had a target role or an extension without
  `schema:`;
- `reset --schemas` on a Supabase project dropped `public`, unless
  `--target supabase` was passed;
- a database deployed with `-e production` was not protected by reset's prod
  guard, and any later run with `-e dev` silently re-marked a prod database
  dev, lifting the guard;
- renaming `project.name` lifted every guard: `reset` with no `--force`
  dropped the tables of a database marked prod;
- `dbd import` truncated and reloaded live tables outside the staging schemas;
- a migration's table drop never ran, while the migration was recorded as
  applied, and `apply -n <entity>` recorded pending migrations it had not run;
- `dbd format` changed string literals and `$tag$` bodies, and `apply`
  rewrote `set search_path to …;` text inside function bodies and literals;
- a SQLite export wrote every BLOB as NULL.

### Changed

- **`-e` takes `dev` or `prod`** (and the aliases `development` and
  `production`), and refuses any other name. Aliases are normalised where they
  are parsed, so bookkeeping records `prod`, never `production`. A name like
  `-e staging` used to be accepted and then matched no `import/<env>/` folder.
- **`dbd reset --target` defaults to the design's target** — its first
  `target:` key — instead of `postgres`. Pass `--target` to override.
- **A materialized view's drift stamp lives in `dbd.matview_stamps`**, not in
  the view's comment, which is now the author's alone. A stamp an earlier dbd
  wrote in a comment is still read until dbd writes a new one.
- **`reconcile` records version 0 for a project with no `project.version`**,
  as `apply` does. It recorded 1, after which a dev `reset` refused as though
  migrations had run.
- **A `-n` that names nothing, or something the scope does not build, is an
  error** — `no entity named 'X'` or `X is outside scope 'S'` — on `inspect`,
  `apply`, `graph`, `export` and `refresh`. It used to succeed having done
  nothing. `Design::apply` and `Design::graph` refuse it too.
- **`dbd release` releases a brownfield project** — one `init --from-db` or
  `merge` gave snapshots but no `released` flag — at the version its snapshots
  reached, without a second baseline. It refuses while the design has changes
  no snapshot records, or when `project.version` disagrees with the latest
  snapshot.
- **Every command counts a scope the same way**: `scope 'X': N of M entities`,
  over the working set including the `deps: include` closure, with externals
  left out — so the unscoped `inspect` count no longer includes them.
- **`init --from-db` gives a Supabase database a `supabase` target** (one with
  both an `auth` and a `storage` schema), and `init --from-dbml` accepts
  `--target supabase`.
- **`dbd combine` writes its generated statements in the project's dialect**: a
  guarded `CREATE SCHEMA` for T-SQL, with every entity's DDL ending its own
  batch with `GO`; nothing for MySQL and SQLite; and PostgreSQL-only extensions
  and roles as a comment.
- **Only a procedure is matched as an import loader, and each staging table
  gets exactly one.** A function was matched too and then run with `CALL`,
  which Postgres refuses for a function. Each table's loader is now, in order:
  the procedure named `import_<table>`; else the only one reading no other
  staging table; else the only one no other table claimed. A procedure loads
  one table and is called once per import; a tie is refused before anything
  runs, naming the candidates. A procedure that reads a staging table without
  loading it, and a function reading one no procedure loads, are named in a
  warning.
- **`import.staging` is enforced.** A data file whose target schema the list
  leaves out is refused — all of them named in one error — before anything is
  truncated, on `import`, its dry run, `deploy` and its dry run. An empty list
  means no restriction. `import.tables` holds per-table options; it does not
  filter which files load.
- **`dbd emit` and `dbd combine` write `-f` and `--report` only inside the
  project**, as `dbml` and `diagram` always did; a bare file name resolves
  against the project root. A path elsewhere overwrote a file dbd does not own
  without a word. Write inside the project and move the file.
- **dbd's JSON import machinery lives in its own `dbd` schema**
  (`dbd.import_jsonb_to_table`), beside the bookkeeping. It was installed in a
  schema called `staging` whatever the project's staging schemas are called —
  undeclared, and visible to `diff` and `init --from-db`. A database an earlier
  dbd imported JSON into keeps its `staging.import_jsonb_to_table`; drop it if
  nothing else uses it.
- **`format` and `inspect --fix` rewrite a file in the encoding it was saved
  in** — UTF-16 behind its byte-order mark stays UTF-16. They wrote every file
  they reformatted as UTF-8.
- **A run with `-e dev` against a database marked prod is refused** — `apply`,
  `deploy`, `reconcile` and `import`, an ad-hoc `import -n -f` and every dry
  run of them included — before any write. It re-marked the database dev,
  which lifted reset's prod guard and loaded `import/dev/` data into
  production. A dev database may be promoted to prod, and a run that promotes
  one now says so; nothing re-marks a prod one. Since `-e` defaults to prod, a
  dev database built without `-e` is marked prod: rebuild it with
  `dbd reset --force`, then a run with `-e dev`. A reset under the all-scope,
  or under the scope the database is pinned to, forgets the database's record
  of the project — env, version and pin; a narrower one keeps it.
- **Renaming `project.name` no longer lifts the guards.** dbd keys its record
  of a database on `project.name`, so a renamed project read a database
  marked prod as one dbd had never seen: `reset` with no `--force` dropped its
  tables, a `-e dev` run passed the environment guard, an older revision the
  version guard, another scope the pin. Now, when the project has no record
  but the database holds its tables, views, materialized views, enums or
  sequences, the other projects' records are the marks every guard reads; a
  refusal names the project that set them and says to set `project.name`
  back. Two projects sharing a database each read their own record, and a
  project the database holds nothing of is held to no other's.
- **An older project is refused on a newer database**: applying a design at v2
  to a database at v3 (or an unversioned design to a versioned database) stops
  before any write, `apply`, `deploy` and their dry runs alike. It said
  "Already up to date (v3)" and re-applied the older DDL.
- **Pending migrations run version by version**, each whole before the next,
  as upgrading one release at a time would: a version's creates and schema
  changes in dependency order, then the data scripts of the tables it alters
  or adds, then its drops, then the record that it ran; after the last
  version, every entity's DDL is re-applied. They ran entity by entity — every
  pending version of one table, then the next table — so a jump across
  versions ran a later version's change before an earlier version's change to
  another table, and a data script reading a column an earlier version adds
  failed on the jump though a stepwise upgrade succeeded. A table one version
  drops and a later one re-adds ends in its re-added shape. A `data.sql` for a
  table its version adds never ran and was not listed; it now runs after the
  version's creates and is listed in the previews.
- **A version that adds enum values can use them.** Postgres refuses a value
  inside the transaction that added it (`unsafe use of new value`), so each
  version's `ALTER TYPE … ADD VALUE` statements — found in the parse tree — run
  at its head as `ADD VALUE IF NOT EXISTS`, and the run commits there. A run is
  one transaction except that, before each version adding enum values,
  everything up to and including those additions commits, with the earlier
  versions recorded. If the rest of that version fails, the values stay and
  the database records the version before it; the next run adds them again as
  no-ops. A run whose pending versions add no enum value is one transaction,
  as before.
- **`apply -n <entity>` is refused while migrations are pending**, naming the
  versions, before any write (the `apply.before` hooks included), and so is
  its dry run. It ran the named table's pending changes and every version's
  drops, skipped the rest, and recorded the versions as applied — after which
  a full apply said "Already up to date" over a missing column. Run
  `dbd apply` first, then `dbd apply -n`.
- **`reconcile` runs the design's grants and its `apply.before`/`apply.after`
  hooks**, scoped as `apply` scopes them, so a reconciled database is the one an
  apply builds.
- **Dry runs ask the database when one is configured** (`-d`, `$DATABASE_URL` or
  `target.url`), and refuse where their real run would: `apply`, `deploy`,
  `import`, `policies` and `reset --dry-run` check the scope pin; `apply`,
  `deploy` and `import` the environment guard; `apply` and `deploy` the
  older-project guard; `apply -n` the refusal over pending migrations; `reset`
  its prod and versioned-database guards. (`reconcile --dry-run` always
  connects, and checks the pin and the environment guard.) `apply`/`deploy
  --dry-run` list pending migrations in the order the run takes them, drops
  marked. An unreachable configured database fails the preview; with none
  configured it stays offline. A dry run never creates a database: a SQLite
  file that does not exist yet reads as none configured, where the preview
  used to create it — and `reconcile --dry-run`, which needs a database to
  compare against, previews against an empty in-memory one, which is what its
  run would create, and refuses where the run refuses.
- **An unreachable PostgreSQL database fails within 5 seconds**, naming the
  `host:port` it tried — never the URL, which may hold a password. sqlx's pool
  retried for 30 seconds and then said only `pool timed out while waiting for
  an open connection`.
- **`import`, `export`, `refresh` and `policies` refuse a database pinned to
  another scope**, as `apply` and `deploy` do. The match is exact (a database
  pinned to `all` refuses `export --scope hub`), and none takes
  `--allow-scope-change`; the message says how to proceed. `diff` and
  `migrate --status`, which write nothing, note the pin instead: an unscoped
  `diff` of a scoped database listed what the scope does not build as missing,
  with no word of why.
- **A scoped `import` or `deploy` refuses a loader the scope leaves out**,
  naming it, before anything is truncated or applied.
- **`export -o <dir>` writes `<dir>/<schema>/<table>.<format>`**, the layout
  `import/` reads, and the default `export/` is the project's (`-s`). Two
  same-named tables in different schemas overwrote one file. The exception is
  a schema folder of the project's own `import/` tree — `import/<schema>` or
  `import/<env>/<schema>` — where each file is written `<table>.<format>`, as
  that import loads it, so `export -n core.accounts -o import/staging` writes
  the `import/staging/accounts.csv` a new database loads into
  `staging.accounts`; two tables that would share one file there are refused
  before anything is written, and the refusal says to export there, with
  `-n`, the one table whose rows that file should hold. Generated columns are
  left out of every export, and an import refuses a file that sets one.
- **A JSONL export writes every column's key, and `null` is SQL NULL** for
  every type. A json/jsonb column holding the JSON value `null` is written
  `{"$json": null}` — as is any value that is itself a one-key `$json`
  object, wrapped once more — and the Postgres and SQLite imports unwrap one
  level, so the JSON value `null` survives a round trip: 0.24.1 wrote both it
  and SQL NULL as `null`, and imported both as SQL NULL. On import, a column
  whose key no record carries takes its DEFAULT; on Postgres a key missing
  from only some records is NULL in those, while SQLite gives each record that
  leaves a key out the DEFAULT.
- **A file directly in `import/dev/` or `import/prod/` belongs to that env**, on
  every engine; on SQLite it loaded in prod. A schema actually named `dev` or
  `prod` can no longer have files directly there.
- **A JSON data file's key that names no column is refused**, as a CSV header's
  is; it was dropped silently. After an import, each identity or serial
  sequence continues past the largest id loaded.
- **`dbd reset --target` takes `postgres`, `supabase` or `sqlite`**, and reset
  works on SQLite. A target that is not the kind of database the connection
  reaches — `--target postgres` on a SQLite URL — is refused before
  connecting; it was accepted and failed on PostgreSQL's `CASCADE`.
- **`dbd emit` for MySQL** writes `LONGTEXT`/`LONGBLOB` for `text`/`bytea`
  (`TEXT` holds 64 KB) and keeps fractional seconds (`DATETIME(6)`, a clock
  default `CURRENT_TIMESTAMP(6)`). `json` is modelled as `json`, not
  `pg_catalog.json`, and maps to each target's JSON type.
- **Key names are kept.** `emit` writes `CONSTRAINT "name"` on primary keys
  (SQL Server and SQLite; MySQL names every primary key `PRIMARY`, which is
  reported), UNIQUE constraints, foreign keys and CHECKs. `init --from-db`
  keeps all four. The DBML round trip (`dbd dbml`, then `init --from-dbml`)
  keeps UNIQUE and CHECK names, and primary-key and foreign-key names other
  than PostgreSQL's defaults (`<table>_pkey`, `<table>_<columns>_fkey`), which
  it leaves out. A key named inline on its column (`id int constraint t_pk
  primary key`) keeps its name too; a column-level `REFERENCES` does not (see
  Known limitations).
- **`dbd emit --dialect tsql` ends every statement with `GO`**, the
  sqlcmd/SSMS batch separator — `CREATE SCHEMA` and `CREATE VIEW` must each be
  alone in a batch. Split on it before sending the script through a driver.
- **`apply` and `combine` keep `public` on a file's own search path by adding
  a statement, and edit none of its text.** After each top-level
  `SET search_path` whose path leaves `public` out, a statement of its own
  sets the same path with `public` added, so an extension installed there
  stays reachable. The statements are found in libpg_query's parse tree, so a
  function's `SET search_path` clause, a body, a string literal and a comment
  are never taken for one. A regex rewrote `set search_path to …;` wherever
  the text appeared, up to the next `;`: a Supabase-style function with
  `SET search_path TO ''` failed with `column "public" does not exist`, and a
  body returning `'g; ok'` deployed returning `'g, public; ok'`. `SET LOCAL`
  and `SET search_path = …` now get `public` too, and a quoted `"Public"` is
  no longer taken for it.
- **`apply` and `deploy` name each DDL file that states no `SET search_path`**,
  with the path its bare names were resolved against, before the run and in
  its preview. Only `inspect` counted them.
- **`dbd format` changes layout and keyword case, and nothing else — and
  formatting twice changes nothing.** Statement boundaries come from
  libpg_query (a `BEGIN ATOMIC` body is one statement). A keyword is recased
  where PostgreSQL's grammar reads it as one — `ATOMIC` kept its case, and so
  did `COLLATE`, `BIT` and `PARTITION BY RANGE` in a statement that was not
  laid out — and a word it reads as a name keeps its case, even a keyword
  elsewhere (a column named `name`, `value` or `level`); built-in type names
  are recased with the keywords. A statement's new layout is used only if it
  parses to the same tree and every string literal, `$$`/`$tag$` body, quoted
  name and comment in it is byte-for-byte the same; failing that the
  statement is only recased, and failing that left as written. A file
  PostgreSQL's scanner cannot read is left unchanged. A comment beside a
  statement stays beside it, and one above it stays above it. It turned
  `E'it\'s; tricky'` into a three-line default, put an added `;` inside a
  trailing comment, dropped a comment after a last statement with no `;`, and
  moved a comment beside a statement onto a line of its own.
- **The DBML reader reads a hand-written document as @dbml/core does**: table
  aliases, an inline `ref:`, multi-line notes and settings, `Note { … }`
  blocks, one-line tables @dbml/core accepts — anything after a column, an
  enum value or a header on its line is refused, naming it — a bare `pk` or
  `unique` after a type, a `pk` index entry (a named primary key),
  `Ref name:` and `Ref name { … }`, an index's `type:` and a table header's
  `[note: …]`. A one-to-one `Ref: a - b` puts the foreign key on `b`, as
  @dbml/core's SQL does (an inline `[ref: - t.c]` puts it on its own column).
  A many-to-many `<>` Ref, which has no foreign key (@dbml/core builds a
  junction table), and a table taking a `TablePartial`'s columns are refused,
  saying why. A note is dedented as @dbml/core dedents one, except that the
  line a closing `'''` sits on is layout, not a trailing line break. Each of
  these was misread or refused.
- **What an extension owns is not the project's.** Introspection leaves out
  every table, view, materialized view, sequence, enum, function and schema an
  extension owns — for `init --from-db`, `diff` and `reconcile` alike. They
  were captured as project files, after which `reset` always failed, and read
  as drift and orphans.
- **Library API.** Embedders of `dbd-core` may need to change code. What
  breaks:
  - `Design::check_scope_guard`, public since 0.24.1, takes `&DatabaseMarks`
    in place of `Option<&ProjectMeta>`. Read the marks with
    `Design::database_marks`, or wrap a row already held with
    `DatabaseMarks::of_own`.
  - `DeployComplete` gains `grants: GrantsOutcome`; `Design::apply_grants`
    (new) applies grants, scoped, and `Design::deploy` calls it.
  - `schema_model::build` returns `Result<SchemaModel>` — it refuses a scope
    whose `deps: include` closure needs something the scope excludes.
  - `DbmlMultiParams` gains a required `design_entities` field.
  - Enums gain variants, so an exhaustive `match` needs an arm: `RefKind::Uses`
    (a column's type, or the sequence its default draws from);
    `ExecutionStep::AddEnumValues` (a version's enum value additions, committed
    ahead of the rest of it) and `ExecutionStep::MigrateData` (its data
    scripts, after every schema change of the version);
    `TableConstraint::Exclude`.
  - Structs gain fields a struct literal must set: `Entity.triggers` (filled
    by the Postgres introspector, written by `emit::emit_table` and
    `emit_view`), `ColumnDef.collation`, `ForeignKey.match_full`,
    `deferrable` and `initially_deferred`, `ReconcilePlan.destructive_changes`
    and `enum_swaps` (`EnumSwap`), `ApplyComplete.previously_built`,
    `Report.out_of_scope_warnings` (a scoped report's `warnings` hold only the
    scope's own), `ReconcileComplete.before_scripts` and `after_scripts`, and
    `ImportScan.unrecognised`.
  - A key named on its column (`constraint t_pk primary key`), or an inline
    `UNIQUE NULLS NOT DISTINCT`, parses to the table-level `TableConstraint`
    it amounts to: a primary key keeps `is_pk` on its column beside the
    constraint; a UNIQUE is the constraint alone, with `is_unique` false.
  - `Design::reset` returns `ResetComplete` (what it dropped, and
    `KeptRole`s); `script::build_reset_script` takes the grants to revoke as a
    final argument.
  - `DatabaseAdapter` gains `forget_project_meta`, which an implementation
    must provide.
  - `ImportPlanEntry.procedure` names only a procedure, and
    `Design::import_data` no longer calls `ensure_import_procedure` — the
    Postgres JSON load installs its own. `plan_enum_recreation` reads indexes
    from the snapshots it is given.
  - `design::import_entry_in_scope` is deprecated in favour of
    `Design::scoped_import_plan`, which also requires a declared staging table
    and says why an entry was left out.

  What is new:
  - `DatabaseAdapter` methods with defaults: `stamp_matview`,
    `forget_matview_stamps`, `truncate_table`, `truncate_tables`,
    `existing_objects`, `roles_held_elsewhere`, `live_matviews`,
    `matviews_reading_columns`, `other_projects_meta`,
    `execute_project_script`, `dependencies` (with `adapter::Dependency`) and
    `foreign_keys` (the keys the database reports, which order a SQLite
    import).
  - The guards: `DatabaseMarks` and `Design::database_marks` (this project's
    record, or the one a renamed `project.name` left); `Design::check_env_guard`
    and `check_version_guard`, which take `&DatabaseMarks` as
    `check_scope_guard` does, and the method `design.check_reset_guard(&marks)`,
    whose refusal calls the database a released project's only when
    `project.released` is set; `Design::conflicting_pin`,
    `check_apply_guards`, `guard_env` (no longer crate-private) and `env`.
  - `dbd_core::connect_existing` and `SqliteAdapter::open_existing`, for a
    caller that must not create a database.
  - `Design::hook_skips` (the hooks a run would skip, decided as the run
    decides them), `unparsed` (what a generated document leaves out),
    `check_import_plan`, `pending_migration_preview` (`MigrationPreview`,
    `MigrationAction`), `roles_kept_by_reset`, `reset_script_keeping`,
    `search_path_warnings`, `apply_hooks_to_run`, `grants_to_apply` and
    `live_import_plan` (the import plan ordered by the database's foreign keys
    too); `DeployComplete::policy_warnings()`;
    `DesignConfig::effective_schema_grants` (what apply grants and reset
    revokes).
  - `script::runnable_ddl`, `runnable_script`, `enum_value_additions`,
    `idempotent_enum_value_additions`, `build_reset_script_keeping`,
    `build_scoped_reset_script` and `build_sqlite_reset_script`;
    `source_text::read_with_encoding` with `Encoding`; `reverse::Declared`,
    `design_yaml_with`, `left_out_schemas`, `outside_needs` and
    `keep_authored_triggers` (what `merge --from-dbml` keeps of a table file);
    `SchemaSelect.supabase`; `doctor::detect_declared_kind_files`;
    `scanner::is_env_folder`; `sql_expr::canonical_exclusion`;
    `reconcile::plan_exclusion_convergence`; `entity::collation_sql`.

### Fixed

**Data safety**

- **`reconcile --prune` no longer reaches into `public`** unless the design
  puts something there. A role, or an extension with no `schema:`, made
  `public` a managed schema, so prune dropped any table there it did not
  declare.
- **`dbd reset --schemas` on a Supabase project keeps `public`.** The protected
  set came from `--target`, which defaulted to `postgres`.
- **A database deployed with `-e production` is protected by reset's prod
  guard**, and so is any existing row that says `production`.
- **A table removed from the design is dropped by the next apply.**
  `dbd snapshot` wrote the drop as `<table>.sql` and apply looked only for
  `<table>.drop.sql`, so no generated drop ever ran — while the migration was
  recorded as applied and its summary counted the drop.
- **A scoped `apply`/`deploy` runs the drops of tables in its schemas.** A
  dropped table is in no scope's working set, so a named scope skipped its drop
  while recording the migration as applied. A drop now runs only where the
  table still exists, and only a drop that ran is counted.
- **`dbd import` no longer truncates tables outside the staging schemas** —
  see `import.staging` under Changed. A data file under `import/app/` emptied
  and reloaded the live `app` table.
- **A database marked prod stays marked prod** — see the environment guard
  under Changed — **and stays guarded after `project.name` is renamed**.
- **A named apply no longer records migrations it did not run** — see
  `apply -n` under Changed.
- **A scoped reset never cascades into what is outside its scope.** It drops
  without `CASCADE`, all or nothing, and names what blocks it; an in-scope enum
  dropped with `CASCADE` took an out-of-scope table's column with it.
- **Reset keeps a role another database on the server holds**, naming the
  database, while still revoking its grants here. It dropped roles another
  database used, or failed claiming dbd had not granted what it had.
- **A materialized view keeps its own `COMMENT`.** dbd's drift stamp shared it:
  the first apply overwrote the author's text, and the next — re-running the
  file's `COMMENT ON` — erased the stamp, after which every reconcile warned.
  On upgrade, a stamp an earlier dbd kept in the comment is moved into
  `dbd.matview_stamps` before anything can erase it.

**Scopes**

- **A scope keeps the sequences of the schemas it keeps.** Sequences were
  dropped from every scope, even an excludes-only one, so a table whose default
  draws from one could not be applied under any scope.
- **A table depends on the enum a column is typed with and the sequence its
  default draws from.** Neither was recorded, so a scope could leave them out
  with no gap reported, and apply failed on a type that did not exist.
- **A trigger written in a table's file makes the table depend on the function
  it executes**, so the function is applied first. The statement was skipped.
- **A scope keeps the schema its extensions install into** (`schema:`), so a
  scoped apply no longer fails with `schema "x" does not exist`. A scope that
  excludes that schema while keeping the extension is refused where the scope
  is resolved, by every command.
- **A scope skips a hook only for what the design manages outside it.** A hook
  touching an `external:` table — or one it creates itself — was skipped under
  every named scope. A skip is now named before the run and in every preview
  (`apply`, `deploy` and `import --dry-run`, `import.after` hooks included);
  it was said only in the summary of a run that succeeded, so a run that failed
  because of it gave no explanation. `import --dry-run` had the rule backwards.
- **A hook's bare table names are judged as Postgres resolves them**: along
  the path each statement runs on (its last `SET search_path`, else
  `source.search_path`, else the session default, with `public` after it), to
  the first schema where the design has that table or view. So a scope skips
  the hook only when what it names really is outside. Its analysis assumed
  `public`, so under a narrow scope a hook naming another schema's table bare
  was not skipped and the apply failed.
- **A scoped migration installs its new roles and extensions**, as a fresh
  scoped install does; a table typed with a new extension's type failed under a
  scope. Its summary counts each entity once.
- **Grants follow the scope**: a scoped apply or deploy grants only on the
  schemas it builds, instead of failing on the rest.
- **A scoped import needs both its staging table and its targets.** It kept an
  entry whose staging table the scope never built, and blamed the staging table
  when a target was the one outside. `import --dry-run` and `deploy --dry-run`
  now name the reason for every entry they leave out.
- **`reset --clean` drops every role the design declares** — `ddl/role/` as
  well as `target.roles` — **and only the extensions the scope installs**.

**Convergence**

- **A `serial` column is not a primary key unless declared one.** The model
  claimed one; reconcile then planned `ADD PRIMARY KEY` beside the real key and
  every real run failed, while `dbml`, the viewer and `emit` showed a key that
  did not exist.
- **A `nextval('s')` default converges.** Postgres reports it as
  `nextval('s'::regclass)`, which read as a changed default on every run.
- **`source.search_path` is the path a file without `SET search_path` runs
  on** — apply, deploy, reconcile and `combine` run
  `SET search_path TO <list>, public` ahead of it. Only the parser used it, so
  the table landed in `public`, `diff` wanted to create it in `app`, and
  reconcile never converged. A migration script stating no path runs on the
  path its entity's CREATE runs on — the last `SET search_path` its DDL file
  states before the CREATE, else `source.search_path` — and the entity's
  references are read on that path too; a hook runs on `source.search_path`.
  With neither set, each runs on the session default (`RESET search_path`).
  Both ran on whatever path the last DDL file had left on the connection, so a
  `data.sql` naming its table bare failed with `relation "notes" does not
  exist`.
- **`diff` and `reconcile` see a dropped index over primary-key or unique
  columns.** Any index over a key's columns was taken for the key's own
  backing index, so dropping it from the database left `diff` empty; the
  backing index is now matched by the constraint's name.
- **Removing an enum value keeps the enum's `COMMENT ON TYPE`**, in
  `reconcile` and in the migration `dbd snapshot` writes. It went with the old
  type, and `diff`, which compares no type comments, could not see it.
- **The enum-value-removal migration `dbd snapshot` writes applies**: two
  stages, the data mapping and then the type swap around the column's views,
  matviews, indexes and defaults. It failed on any view over the column and on
  an enum default.
- **`reconcile` rebuilds the partial and expression indexes that read an enum
  column** around its type swap; it failed with `operator does not exist`.
- **A pre-existing, unstamped materialized view is stamped** when its deployed
  query is the design's (same parse tree, same index keys), so a brownfield
  project diffs clean.

**Commands**

- **`dbd deploy` applies grants**, between the schema and the data. They ran
  only inside `dbd apply`'s handler.
- **`dbd deploy` honours `-c`.**
- **`dbd export` writes the views listed under `export:`** — views that
  dereference foreign keys so the files match the import staging tables, the
  way a long-running system's data moves to a new database. A listed view was
  filtered out as "not a table", silently; an entry naming nothing exportable is
  now reported.
- **`dbd snapshot` and `dbd release` edit `design.yaml` in place**, keeping its
  comments, blank lines and flow-style lists. They re-serialised the file.
- **`GITHUB_TOKEN` authenticates GitHub downloads**, so `dbd deploy` can fetch a
  private repository. It was documented and never sent; it goes to
  `api.github.com` only.
- **A GitHub `--source` outside `dbd deploy` is refused**, with the way out:
  clone the repository and pass its path.
- **`dbd init --target` refuses a target it has no scaffold for**, instead of
  silently writing a PostgreSQL project.
- **`dbd doctor --fix` migrates every folder alias the scanner reads**:
  `materialized_views`, `matview`, `matviews` and `sequences`.
- **`--help` for `--source` and `dbd diff`** is accurate.
- **The default `dbd reset` works on a design with schema grants.** It ran
  `DROP ROLE` while the roles still held the grants dbd had made, so the whole
  reset rolled back. It now revokes what dbd granted — the `schemas:` and target
  `grants:`, and the `ALTER DEFAULT PRIVILEGES` the resetting user set — on every
  schema that survives, then drops the role. A role another database on the
  server holds is kept and named; one holding anything else here fails the
  reset with an error naming it and what it holds; dbd never runs
  `DROP OWNED BY`.
- **`reset --dry-run` refuses where the real reset would** — a prod database,
  applied migrations, a database pinned to another scope — whenever a database
  is configured, with `--force` and `--allow-scope-change` honoured the same
  way. With none configured it stays an offline preview.
- **`reset` forgets the drift stamps of the materialized views it drops.**
- **`reconcile` creates entities in the dependency order `apply` uses**, so a
  table whose default or trigger calls a project function comes after it. On a
  fresh database it failed partway, leaving the tables it had created.
- **Removing an enum value drops and recreates only the views over the enum's
  tables.** Every entity referencing the table — tables, procedures, functions
  — was dropped as a view, and the run failed with `"x" is not a view`.
- **`reconcile` refuses an enum value removal that a live materialized view
  blocks** — one reading the column, directly or through views — before
  writing anything, under `--dry-run` too, and exits non-zero. The refusal
  names each matview and the way through: drop it by hand
  (`DROP MATERIALIZED VIEW …`), then re-run `dbd reconcile
  --allow-destructive`, which removes the value and re-creates the matviews
  the design declares. It decided from the design instead: with a matview over
  the column declared, it planned nothing for the enum, warned, and exited 0
  "Reconciled"; following its advice, the re-run re-created the matview and
  declined again.
- **The destructive-reconcile refusal lists what is destructive** — each drop,
  by entity — and nothing else. It listed every altered table, ones that had
  only gained a column included.
- **A row holding a removed enum value fails naming its column**, not only
  the value.
- **`reset` refuses a versioned database as "versioned at vN"**, calling it a
  released project's database only when `project.released` is set. It said
  "has applied migrations" of a fresh install that had run none.
- **`dbd combine` writes the `search_path` apply runs** — `public` kept on
  the path after each top-level `SET search_path`, by a statement of its own —
  so `psql -f` on its output no longer fails where apply succeeded.
- **Every command that writes to or loads from a database refuses one pinned
  to another scope** — see Changed for `import`, `export`, `refresh`,
  `policies` and the dry runs; `diff` and `migrate --status` note the pin.
- **`import -n` naming no data file is an error before anything runs.** It
  exited 0 and still ran `import.after`.
- **`init --from-db` keeps a view's and a materialized view's comment, and a
  CHECK's name.** A legacy drift stamp in a matview's comment stays out.
- **The formatter's `keyword_case` does what it says in all three modes**, for
  every keyword in every statement — `GENERATED`, `IDENTITY`, `RESTRICT`,
  `VALID`, index methods and built-in type names included. It wrote
  `GENERATED by default as IDENTITY`; `preserve` uppercased a lowercase file;
  `upper` left `SET` and tables with a `serial` or `inet` column lowercase.
  String literals, `$tag$` bodies, quoted names and comments are left alone —
  see Changed.
- **`dbd format` lays out a table whatever its literals and names**: a
  `U&'…'` or `$$` default, a quoted table or column name, a column's own
  CHECK, a named column constraint, and `TEMP`, `WITH (…)`, `PARTITION BY`,
  `INHERITS` and `TABLESPACE` around the column list. A `U&'…'` default was
  laid out re-spelled (`U&'dat'`); each of the others kept the table from
  being laid out.
- **`dbd reset` prints what it dropped**, counted by kind, or that it found
  nothing; it printed nothing.
- **`dbd graph` names a file it leaves out because it does not parse**, on
  stderr, and leaves it out of the nodes, layers and edges, as it says; `-v`
  counts go to stderr too, so `dbd graph -v | jq` works.
- **An unset or empty `$DATABASE_URL` is named**, with how to set it, instead of
  `relative URL without a base`; an exported-but-empty one falls back to
  `target.url`.
- **`dbd … | head` ends quietly** (exit 141) instead of panicking on a closed
  pipe.
- **`inspect -n <external>` and `graph -n <external>` refuse**, as `apply -n`
  does; `doctor` counts what it fixed the way it counts what it found; a file
  that does not parse gets no false "no SET search_path" warning; the
  extension-schema refusal says each clause once.

**Dry runs and `inspect` agree with the real run**

- **`apply --dry-run` refuses what `apply` refuses** — a file that does not
  parse, a scope gap or closure conflict, an unknown `-n` — and honours the
  scope's `extensions:` allowlist. It said "no issues" and exited 0.
- **`deploy --dry-run` refuses a file that does not parse**, and reports the
  scope it runs, its entity count and the policies it would apply, after scope
  filtering. Under `deps: include`, auto-included gaps print as `+
  auto-included:`, not as failures.
- **`policies --dry-run --scope` lists only the policies the real run
  applies.**
- **`reconcile --dry-run` refuses a database pinned to another scope**, as the
  real run does, and lists the `apply.before`/`apply.after` hooks and the
  grants the run goes on to apply. It stopped at the schema changes.
- **`apply` and `deploy --dry-run` refuse a pending migration with an
  unresolved `data.sql` TODO**, as the run does, before printing any of the
  plan, and list the migration steps in the order the run takes them.
- **A scoped dry run whose pending migrations touch nothing in its scope says
  the run records them** and moves the database to the design's version. It
  said "No pending migration to run (database at v4, design at v7)", and the
  run then said "Migrated v4 → v7".
- **`inspect --scope` refuses a scope whose `deps: include` closure needs what
  it excludes**, as apply, `dbml` and `diagram` do, and runs its advisory checks
  over the scope rather than the whole design.
- **`inspect --fix --scope` formats only the scope's files**; `import -f
  --scope` refuses a declared table outside the scope; `--deps` given to a
  command that takes no scope is warned about, as `--scope` is.
- **`inspect -s <relative path>` finds the project's files.**
- **Output says what happened.** `deploy` reports each phase in the order it
  ran; `policies --dry-run` lists skips under their own heading; `export`
  counts tables and views apart and names what the scope dropped; `inspect -n`
  counts and advises on the named entity alone; `inspect --scope` keeps
  out-of-scope warnings apart; every scoped command prints its
  `scope 'X': N of M entities` line (`diff --json` on stderr); an unknown scope
  with none defined lists only `all`.
- An apply on a database already built but unversioned says so, not "Fresh
  install"; `migrate --status` says "up to date" only when the versions match;
  an unversioned apply no longer says "Fresh install at v0"; `doctor` counts
  what it checked; the refusal `diff`/`reconcile` give a T-SQL or MySQL project
  no longer claims `apply` works there; the Supabase scaffold's `ignore`
  patterns match Supabase's schemas.

**Import and export**

- **The default `truncate: true` works on SQLite and Convex.** Import issued
  `TRUNCATE`, which SQLite does not have and Convex cannot run; truncation now
  goes through the adapter (`DELETE FROM` on SQLite, `--replace` on Convex).
- **A `.json` data file is read as JSON** — an array of records or JSON lines —
  on Postgres, SQLite and `import -f`. It was read as CSV.
- **A JSONL export imports back to the same values**: backslashes are no longer
  doubled (it was written from `COPY` text); a json/jsonb string scalar, the
  JSON value `null` kept apart from SQL NULL (see Changed for how each is
  written) and a json column's exact text survive; identity ids,
  `GENERATED ALWAYS` included, import as given; generated columns are left out
  and recomputed.
- **`dbd export` refuses a format Postgres cannot write**, instead of writing
  CSV under the requested name.
- **An ad-hoc JSON import runs on a fresh database**: the Postgres JSON load
  installs its own loader procedure.
- **A Postgres CSV/TSV import loads the columns its header names**, as SQLite
  does. It loaded by position, so a file with its columns in another order put
  values in the wrong columns. A header column the table does not have, or
  names twice, is refused; CRLF and a byte-order mark are handled.
- **SQLite reads CSV/TSV as RFC 4180** — quoted commas, newlines and doubled
  quotes; nothing trimmed; a quoted field is never NULL — so a Postgres export
  imports unchanged. A quoted comma was rejected and quotes were kept.
- **A SQLite CSV/TSV export imports back to the same values**, here or into
  Postgres. Cells were joined unquoted, so a comma or line break in a value
  broke the row, and NULL and `''` both came out empty; they are now written as
  Postgres's `COPY` writes them — with the table's header even when it has no
  rows, and without its generated columns. A BLOB is written as `\x…` hex, as
  Postgres writes a `bytea`, in every format, and read back into a BLOB
  column; every BLOB was exported as NULL.
- **Staging tables linked by a foreign key import together.** Postgres empties
  them in one `TRUNCATE` and loads a referenced table before one that
  references it; a table outside the import holding a key to one of them fails
  the import, named, with nothing emptied. The truncate failed naming no
  table. SQLite, whose DDL dbd reads verbatim, takes the order from the
  foreign keys the database reports: it loads a referenced table first and
  empties them one at a time, referencing table first, so an import, a
  re-import and `deploy` succeed. `import --dry-run` lists that order when the
  database exists. They were ordered alphabetically and failed on the key.
- **A data file whose target is a view, a materialized view or a sequence is
  refused** before anything is truncated, the refusal saying why for each kind
  it names.
- **A data file in a folder deeper than an import reads** —
  `import/staging/sub/x.csv`, `import/dev/staging/sub/y.csv` — is never
  loaded, and is named with the layouts an import does read. The first was
  reported as skipped for an env named `staging`, as though some `-e` could
  load it; the second loaded into `staging.y` under `-e dev`, its `sub` folder
  ignored.
- **Import counts tables loaded, not data files**, and an ad-hoc `import -n -f`
  and its dry run print the scope line.
- **A JSON record mentioning `set search_path to …` imports unchanged.**
  Records are staged as bound parameters, not spliced into SQL that the
  search-path rewrite then edited.
- **A UTF-16 DDL file applies, deploys and combines.** The scan decoded it, but
  apply, deploy, combine, format, doctor, merge and the migration readers read
  it as UTF-8 and silently skipped or garbled it.

**`dbd emit`** — what a target cannot take is reported, inline and in
`--report`.

- A name holding the target's quote character is escaped as the target
  escapes it: SQLite doubles `"`, MySQL a backtick, SQL Server `]`. A name
  holding one ended early (`"app_we"ird"`) and the script did not run.
- Foreign keys, CHECK constraints and indexes are carried. A CHECK expression or
  index predicate goes across untranslated and is reported; an index the target
  would reject is left out and reported.
- Identity and `serial` columns are numbered by the target (`AUTO_INCREMENT`,
  `IDENTITY(1,1)`, `INTEGER PRIMARY KEY AUTOINCREMENT`).
- Generated columns are computed by the target, their expression reported as
  untranslated.
- Defaults with an equivalent are translated (`now()`, `current_date`,
  `gen_random_uuid()`); the rest are reported. A literal's `::cast` — a syntax
  error on every target — is dropped.
- SQL Server gets `CREATE SEQUENCE` with PostgreSQL's bounds, and
  `NEXT VALUE FOR`; elsewhere a sequence and its `nextval` defaults are reported.
- Comments are carried where the target keeps them (MySQL `COMMENT`, SQL
  Server `MS_Description`) and reported elsewhere.
- A materialized view's downgrade to a view, and each index it loses, are
  reported. A key column on unbounded text is bounded, with a report entry,
  rather than emitted as a key the target refuses. An FK to a table the script
  does not create is left out and reported. A file that does not parse is
  reported instead of vanishing. SQL Server gets `CREATE SCHEMA`.
- A trigger written in a table's file is reported as skipped, as routines are.
  Extensions, roles, grants (per schema) and RLS policies (per file) are
  reported as not emitted.
- `char(n)`, `real`/`float4` and `time` map to the target's own type;
  `timetz` becomes the target's `time` on MySQL and SQL Server, its lost zone
  reported; `interval`, `inet` and `cidr` become text, reported with the real
  reason. "If it is an enum" is said only of a declared enum.
- A `time` column's clock default (`now()`, `current_time`, `localtime`) is
  the time of day: SQLite `CURRENT_TIME`, MySQL `(CURRENT_TIME(p))`, SQL
  Server `CONVERT(time, SYSDATETIME())`. SQLite and MySQL were given
  `CURRENT_TIMESTAMP`, which MySQL refuses on a `TIME` column.
- A UNIQUE's lost `NULLS NOT DISTINCT` is reported on MySQL and SQLite, whose
  UNIQUE lets NULL keys repeat. A foreign key's `DEFERRABLE INITIALLY
  DEFERRED` is carried to SQLite; an EXCLUDE constraint, a column collation, a
  `MATCH FULL` foreign key and, outside SQLite, a deferred one are reported as
  lost.
- On SQLite, a view's query and a CHECK are written again from the parse tree:
  `core.cities` reads the flattened `core_cities`, `POSITION(… IN …)` becomes
  `instr()`, and PostgreSQL's NULL order, `LIKE` escape and column names are
  written out. A construct SQLite cannot run with the same meaning leaves the
  view or CHECK out, reported — so the script applies. Needs SQLite 3.39+.
- A type past the target's documented limits is switched or clamped and
  reported: `nvarchar(n)` past 4,000 → `nvarchar(max)`; MySQL `VARCHAR(n)` or
  `CHAR(n)` past 16,383 → `MEDIUMTEXT`/`LONGTEXT`; `numeric(p,s)` to MySQL's
  65,30 or SQL Server's 38; SQLite reports a `numeric` it may round — one
  whose values can pass 15 significant digits with a fraction, or 18 whole
  digits; a row too wide for the target gives up its widest non-key columns —
  to TEXT on MySQL, and from `nchar` to `nvarchar` on SQL Server. An unbounded `varchar` becomes the
  target's unbounded text instead of a silent `VARCHAR(255)`.

**`dbd dbml` and `init --from-dbml`** — the round trip keeps what DBML can say.

- Table-level UNIQUE constraints (as unique indexes marked `unique constraint`,
  read back as constraints with their names), expression indexes
  (backticked), CHECK constraints (a `checks` block), generated columns and an
  ALWAYS identity (custom properties), and a column's `unique` and `increment`.
- A partial index's predicate, key order and `NULLS NOT DISTINCT` travel in the
  index note (`where:`, `order:`, `nulls not distinct` lines).
- Every referenced table the document does not define gets a stub, typed from
  the design. A `--scope` document did not parse at all: DBML rejects a Ref to
  an undefined table.
- Strings are escaped as DBML's lexer reads them, and the whole document is
  lexed by @dbml/parse's token rules, so a quote or backslash in a note or
  default survives — an odd number of `"` in a `'…'` note included — and a
  column comment keeps its line breaks.
- Every name is written in DBML's quoting and read back as itself — schema,
  table, column, enum and enum value, index, primary-key, foreign-key
  (`Ref "name":`), UNIQUE and CHECK names — whatever it holds: a quote, a
  backslash, a space, a dot, a `'` or non-ASCII text. An enum column's type is
  DBML's own reference to it, `"app"."mo\"od"`. A quote in a name ended its
  DBML string, so the document did not parse, and an enum column's type was
  written in SQL quoting, which @dbml/core refuses. A `"` inside a `'…'`
  setting no longer ends the settings (the table lost its primary key), and a
  Ref endpoint, an index tuple and a Ref name read their escapes. The DDL
  `init --from-dbml` writes doubles a quote in every identifier, `CREATE
  SCHEMA` included. (A schema, table or enum name also names a file, so a
  backslash in one is refused; for a dot in one, see Known limitations.)
- An index's method (`type:`), and its `INCLUDE` columns, operator classes and
  storage parameters (`include:`, `opclass:` and `with:` lines in its note),
  are written and read back, as are a named primary key (a `pk` index entry
  with its `name:`) and a named foreign key (`Ref "name":`); PostgreSQL's
  default names are left out.
- An enum column's type is written qualified, as the table's `search_path`
  resolves it: `init --from-dbml` then `apply` failed with
  `type "mood" does not exist`. A CHECK keeps its name in the rebuilt DDL.
- A stub is typed like the foreign key that references it and declares each
  referenced column set unique, so `dbml2sql`'s SQL applies; an `external:`
  stub was `varchar` against a `uuid` key.
- `dbml` and `diagram` name each file they leave out because it does not
  parse, and still write what they can. They left it out without a word.
- A SQL value keyword default (`current_date`, `current_user`, …) is written as
  an expression, decided from its parse tree; it became the string
  `'current_date'`. A multi-line note reads back exactly in @dbml/core.
- A stub reads back as `external:`, not as a table to build: `init
  --from-dbml` wrote one as a one-column managed table and added its schema to
  `schemas:`, so the next apply built a copy of somebody else's table.

**`dbd init --from-db`** — a reversed project of what dbd models inspects
clean and applies, to its own database and a fresh one, with an empty `diff`
— under `--schema`, to a database that has what the run names as needed. Much
of what dbd's model cannot hold is now named instead of weakened in silence;
what is still dropped without a word is under Known limitations.

- Extensions go in the target's `extensions:` list and schemas in `schemas:`;
  it wrote `ddl/extension/` and `ddl/schema/` files the scanner refuses, so
  `inspect` and `apply` failed on any database with an extension. `doctor`
  names the design.yaml entry for such a file.
- The DDL is re-runnable (`IF NOT EXISTS`, `OR REPLACE`), so the documented
  brownfield path — reverse, then apply to the same database — no longer fails
  with `already exists`.
- Supabase's platform schemas are left out only on a Supabase database; on
  plain Postgres `auth`, `extensions`, `cron` and `net` were dropped without a
  word. Every schema and extension left out is named, and a table a foreign key
  reaches outside the project is declared `external:`.
- Key names, enum comments and view and matview comments are kept.
- A table's triggers are captured into its file — `CREATE OR REPLACE
  TRIGGER`, or `DROP TRIGGER IF EXISTS` first for a constraint trigger, which
  Postgres will not replace — and a view's `INSTEAD OF` triggers into the
  view's, which is then applied after the function they fire. A disabled
  trigger is named. Triggers were not captured, and `merge` deleted them from
  the table files it overwrote. `merge --from-dbml`, whose document cannot
  hold a trigger, keeps the triggers written in each table file it rewrites,
  with the `SET search_path` they were written under. A SQLite table's
  triggers are captured too.
- EXCLUDE constraints, column collations (`COLLATE`), and a foreign key's
  `DEFERRABLE`, `INITIALLY DEFERRED` and `MATCH FULL` are modelled —
  introspected, parsed, compared by `diff`, converged by `reconcile`, emitted.
  An EXCLUDE was captured as a plain gist index (the speed kept, the rule
  gone), the parser refused one in a project, and the rest were lost — each
  with an empty diff.
- A partitioned table is captured as one plain table holding every row, its
  partitions folded into it, with a warning naming its key and each
  partition's bounds. Each partition was captured as a table of its own
  beside a plain copy of its parent, so an export held every row twice. A
  constraint's `NOT VALID`, `NOT ENFORCED` or `NO INHERIT`, a primary key's or
  UNIQUE's deferral, `UNLOGGED` and inheritance are each named, for the table
  or constraint that has them. A `NOT VALID` CHECK no longer reads back as
  `check (check (…) not valid)`.
- Objects an extension owns are left out (see Changed).
- `--schema X` declares the extensions the captured objects use, when they
  are installed in `public` or a captured schema, and prints a `needs:` line,
  naming the objects that use it, for each other type, function or table they
  need from outside — Supabase's platform schemas aside. It left them out
  without a word, and the project failed on a new database.
- A schema nothing was captured in is left out of `schemas:` unless
  `--schema` named it. An empty `public` was listed, and `reset --clean`
  would have dropped it.
- A reversed enum is written inside `DO … EXCEPTION WHEN duplicate_object`,
  so `psql -f` runs its file twice.
- `merge` never captures a table declared `external:` — it names it, left
  out — so somebody else's table (the Supabase scaffold's `auth.users`) never
  becomes a project file that `reset` would drop; and it never tells you to
  declare external a table the project builds, as a scoped DBML document's
  stubs would suggest. It warns about undeclared extensions and names only the
  schemas it wrote files into.

**The schema viewer**

- Every foreign key lands somewhere: on its table, or on a stub marked
  external, out of scope or unresolved. It used to lead nowhere.
- Tables show their CHECK constraints, index predicates, and identity and
  generated columns; sequences have pages of their own.
- A schema holding only views or routines is listed.
- Under `--scope`, the changelog covers the same working set as the diagram.
- `dbd diagram` refuses a scope that excludes its own dependency, as every
  other command does, instead of drawing an empty model.
- The viewer refuses a model newer than it reads, instead of rendering it with
  whatever the new version added silently dropped.

### Known limitations

- On Convex, `truncate: false` has no effect (the import always replaces), and
  a `.json` file must be a JSON array — Convex's importer reads no JSON lines.
- Import data files are read as UTF-8; a UTF-16 CSV is not decoded.
- A scoped apply still syncs pg_cron refresh jobs for the whole design, so a
  scope whose `extensions:` allowlist leaves out pg_cron fails if a
  materialized view outside it has a schedule.
- `dbd combine` leaves out an entity whose file does not parse, without saying.
- `reconcile` is not one transaction: each step commits as it runs, so a run
  that fails partway leaves its earlier steps applied. (An enum value added in
  a transaction cannot be used until it commits, so the run cannot be wrapped
  in one.)
- On MySQL and SQL Server, `emit` copies a view's query and a CHECK verbatim
  (reported); on SQLite, generated-column expressions and index predicates are
  copied verbatim. A `CREATE VIEW v (a, b)` column list is lost on every
  target.
- `emit` does not report a `timestamptz` losing its zone on MySQL
  (`DATETIME`) or SQLite (`TEXT`), nor check MySQL's 3,072-byte or SQL
  Server's 900-byte index-key limits. SQL Server's UNIQUE lets only one row
  hold a NULL key, which a PostgreSQL UNIQUE over a nullable column does not
  limit; that is not reported either.
- A foreign key named on its column (`pid int constraint p_fk references …`)
  loses its name in `emit` and in DBML.
- `dbd diff` reports permanent drift on a negative default (`-1` against
  Postgres's `'-1'::integer`), on a default holding a backslash (it wants
  `e'a\\b'` where Postgres reports `'a\b'::text`, even right after `init
  --from-db`) and on `varbit` columns. It prints identifiers unquoted
  (`ALTER COLUMN my col`), so a statement it shows for a name that needs
  quotes does not run as printed.
- `init --from-dbml` names every unnamed index `<table>_idx`, so a second
  unnamed index on one table collides on apply. dbd's DBML reader takes a
  one-line note's leading whitespace off, as @dbml/core does. The DBML
  writer leaves out a foreign key's deferral and `MATCH FULL`, EXCLUDE
  constraints and column collations, which DBML has no syntax for, so they do
  not survive the round trip. An expression default or CHECK holding a
  backtick is written into a document `init --from-dbml` refuses (DBML has no
  escape for one), and a line break inside a CHECK's or index predicate's
  string literal comes back as a space.
- A snapshot's enum-value-removal migration does not see enum array columns,
  and leaves the renamed old type behind when the same snapshot drops the
  table; a dependent view in a scope the database does not build is dropped
  and not recreated.
- `reconcile` records the project's version over a newer one (the older-project
  guard covers `apply` and `deploy`).
- Concurrent JSON imports into one database share `dbd.import_tmp`.
- After `init --from-db`, `inspect`, `apply` and `deploy` note "no SET
  search_path" on every file: the reversed DDL is fully qualified.
- `merge --schema` does not print the `needs:` lines `init --from-db --schema`
  prints. An export of a table others inherit from holds their rows too
  (`init --from-db` warns of it). A `json[]`/`jsonb[]` element that is SQL
  NULL comes back from a JSONL round trip as the JSON value `null`.
- `apply.before` hooks run outside apply's transaction, so their effects stay
  when the entity batch rolls back. A run whose pending versions add enum
  values commits at each such version (see Changed), so a later failure rolls
  back only to there.
- On a jump across versions, a table an earlier pending version adds is
  created from its current DDL, so a later pending version's migration adding
  a column to it fails with `already exists`; and a table one version alters
  and a later one drops is not altered on the way. Upgrade one release at a
  time, or write the column additions `IF NOT EXISTS`. `apply`/`deploy
  --dry-run` do not list the hooks and grants the run applies, as
  `reconcile --dry-run` does.
- A released project never drops a view, function or sequence removed from
  the design: snapshots hold tables and enums only.
- A renamed `project.name` whose run trips no guard is recorded beside the old
  name's record, which stays; and a renamed design ahead of the old record's
  version is planned as a fresh install, so its pending migrations do not
  run. Set `project.name` back before upgrading.
- `dbd format` lays out only a table sqlparser 0.62 can read; one it cannot —
  `UNLOGGED`, `LIKE`, `EXCLUDE`, `USING <method>`, `OF <type>`, `PARTITION
  OF`, identity options beyond one (`(START WITH 1 INCREMENT BY 1)`), a key's
  `INCLUDE`/`WITH`/`USING INDEX TABLESPACE`, `NO INHERIT`, `ON DELETE SET
  NULL (<columns>)`, a column's `STORAGE`/`COMPRESSION`, an `ARRAY` type
  (`int ARRAY`), `WITHOUT OIDS`, `U&'…' UESCAPE` — is recased, not laid out;
  so is an enum type with a label holding a `'`. A word on the formatter's own
  list (`data`, `type`, `key`, `comment` …) is recased even where it names a
  column.
- `init --from-db` neither captures nor names a domain or a composite type,
  so a project using one fails on a fresh database (`type "app.posint" does
  not exist`). It drops without a word an identity column's `START` and
  `INCREMENT`, a table's storage parameters (`fillfactor`), a column's
  `STORAGE`, row-level security and its policies, and comments on
  constraints, indexes and functions.
- A dot in a schema, table or enum name is not supported: dbd joins a schema
  and a name with a dot and splits them apart again, and `emit`, for one,
  writes a table `app."we.ird"` as `app_ird`.
- `dbd diff` does not compare table, view or enum comments.
- `emit` loses an identity column's `START`/`INCREMENT` options; its MySQL and
  SQL Server output is written from documented syntax, not run against a server.
- DBML cannot express an enum's comment.
- How Postgres renders a name inside a live default depends on the reading
  session's `search_path`, which a pooled connection can inherit from an
  earlier DDL file; a bare `nextval('s')` in a file can still read as drift.

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
[Unreleased]: https://github.com/sensei-hq/dbd/compare/v0.25.1...main
[0.25.1]: https://github.com/sensei-hq/dbd/releases/tag/v0.25.1
[0.25.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.25.0
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
