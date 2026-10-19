# Backlog

Pending and future work only. **Shipped features are documented in the
[guides](guide/)** — the [commands reference](guide/04-commands.md), the
[design.yaml reference](guide/03-design-yaml.md), and
[snapshots & migrations](guide/05-snapshots-migrations.md) — with internals in
the [architecture notes](design/architecture.md). They are not tracked here.

## 0.25.1 — defects found while auditing 0.25.0

Found by the 0.25.0 audits and not fixed in it — older code, not introduced by
the release, except the entries marked *(0.25 code)*, which sit in code the
release added. Each was reproduced or read in the code; locations are as of the
0.25.0 branch.

**Migrations and snapshots**
- A jump over a table created in one version and altered in a later one fails
  with "column already exists" (creates use the current DDL); a table altered
  in one version and dropped in a later one is not altered during a jump.
- A fresh v0 install is refused over a `-- TODO` in a migration it never runs
  (`design/apply.rs` ~286-291).
- `snapshot` writes a bare `ADD COLUMN … NOT NULL` with no default and no
  `data.sql` TODO, which fails on a populated table.
- On SQLite, `release`/`snapshot` capture no tables (`snapshot.rs` ~361-371
  needs a `table_def`), so `snapshot` never sees a change.
- `migrate --status` lists every migration as pending on a fresh database, where
  apply does a fresh install.
- A released project never drops a view, function or sequence removed from the
  design (snapshots hold tables and enums only).
- `reconcile` has no older-project guard and records the project's version over
  a newer one.
- A `data.sql` for a table its version neither adds nor alters (one it drops,
  say) never runs, and no preview lists it (`design/plan.rs` ~268).

**Guards and bookkeeping**
- A renamed `project.name` whose env, scope and version all match the old record
  trips no guard and leaves two records; a renamed design ahead of the old
  record plans a fresh install; a forced reset under a renamed project leaves
  the old record (`design/reset.rs` ~96-99).
- `init --from-db`'s "managed by dbd (version 0)" reads the meta row under the
  new `--name` (`src/commands/reverse.rs` ~159).
- reset's scope-guard message says "Applying a different scope".

**search_path**
- A script whose `SET search_path` comes after its first statement runs that
  statement on the leftover path (`script.rs` ~119-127).
- With no `source.search_path`, a pathless DDL file runs on the previous file's
  path, while the warning says "session default" (`script.rs` ~134-140).
- The "no SET search_path" warning fires for a file whose only statement is
  `SET … TO DEFAULT` or `RESET` (`design/mod.rs` ~632).
- Policy files run on the pooled connection's leftover path (`design/mod.rs` ~533).

**Reconcile and diff**
- Reconcile can't remove an enum value that is the column's live default; it
  re-sets the live default, not the design's (`reconcile.rs` ~1440-1455).
- Reconcile never creates a trigger added to a table file, and `diff` doesn't
  compare triggers.
- `diff` says "cannot drop FOREIGN KEY … no constraint name" for named FKs and
  writes EXCLUDE unnamed (`diff/generate.rs` ~157-168, 240); it prints
  identifiers unquoted.
- Permanent drift on a negative default (`-1` vs `'-1'::integer`), a default
  holding a backslash, and `varbit` columns.
- `reconcile --dry-run --allow-destructive` still says "re-run with
  --allow-destructive".

**SQLite**
- `diff`, `export` and `migrate --status` create a missing SQLite file, where a
  dry run opens only one that exists (`src/commands/diff.rs` ~80, `data.rs`
  ~332, `migration.rs` ~279 — each through `get_adapter`).
- `init --from-db` and `merge` write a table or view as `sqlite_master` holds
  it, with no `IF NOT EXISTS`, so applying the project back to its own database
  fails (`table log already exists`) (`adapter/sqlite.rs` ~618, 635).
- *(0.25 code)* An import empties its tables one `DELETE FROM` at a time, each
  committed as it runs, so a failure partway leaves the tables before it empty
  (`adapter/mod.rs` ~246-253).
- `import --dry-run` with no database to ask lists a SQLite project's tables
  alphabetically, not in the foreign-key order the import takes
  (`src/commands/data.rs` ~150).

**Quoting**
- Names interpolated into SQL without doubling a `"`: `script.rs` ~566-569
  (`CREATE ROLE`, `GRANT` role, and the `rolname = '…'` check, which doubles no
  `'` either), ~919-924 (`REVOKE`), ~962 (`DROP ROLE`), ~1031 (the reset
  script's `DROP … CASCADE`), ~1048 and ~1057 (`GRANT USAGE`); `reconcile.rs`
  ~1245 (comment convergence), ~1758 and ~1767 (index convergence);
  `diff/generate.rs` ~236 (`DROP CONSTRAINT`) and ~258 (`DROP INDEX`).
- A dot in a table or enum name: dbd joins a schema and a name with a dot and
  splits them again at the first (`entity::split_qualified_name` and its kin),
  so the name reads right, but `emit.rs` `bare()` (~16) cuts at the last dot,
  so `app."we.ird"` is emitted as `app_ird`. A dot in a schema name is refused,
  named (#89).
- A type in a schema that needs quoting never matches its introspected
  spelling: `format_type` writes `"Ext".citext`, `reconcile::canonical_type`
  lowercases inside the quotes (`"ext".citext`), and the enum map and
  `qualify_extension_types` build `ext.citext` unquoted — so an enum or an
  extension type in a mixed-case schema reads as drift on every run, and the
  emitted `TYPE ext.citext` names a schema that does not exist. Needs
  quote-aware case folding in `canonical_type` and `sql_quote::ident` for the
  canonical names (found reviewing #41).

**Reverse engineering (`init --from-db`, `merge`)**
- Domains and composite types are neither captured nor named, so apply on a
  fresh database fails; identity `START`/`INCREMENT`, fillfactor, column
  `STORAGE`, RLS, and comments on constraints, indexes and functions are
  dropped silently.
- A table whose name holds a backslash is skipped ("unsafe path segment") and
  not declared external.
- `merge --schema` doesn't use the dependency report `init --schema` prints, and
  says "left out: extension citext" while keeping it declared.
- A second `CREATE TABLE` in one table file merges into the entity (emit writes
  a duplicate column).
- Exports of an inheritance parent include the child tables' rows (warned).

**emit and DBML**
- The DBML writer drops FK deferral, `MATCH FULL`, `EXCLUDE` and collation (DBML
  has no syntax for them); `init --from-dbml` names every unnamed index
  `<table>_idx`, so a second one collides on apply.
- `emit` doesn't check MySQL's 3,072-byte or SQL Server's 900-byte index-key
  limits; SQL Server's plain UNIQUE over a nullable column admits one NULL,
  unreported; SQLite doesn't report `timetz` losing its zone; T-SQL writes an
  empty `GO` batch for an extension comment.
- `json[]`/`jsonb[]` elements can't tell a JSON-null element from an SQL NULL.
- *(0.25 code)* The DBML writer can't write an expression default or CHECK
  holding a backtick (DBML has no escape; `init --from-dbml` refuses the
  document), writes a line break inside a CHECK or index predicate as a space —
  changing a string literal that holds one — and reads `with:` storage options
  split on commas (`dbml.rs` ~541-545, ~776, ~835-840).

**Formatting**
- A word on the formatter's own keyword list (`data`, `type`, `key`, `comment`
  …) is recased even where it names a column — `DATA JSONB` in `upper` mode
  (`formatter/keyword_case.rs` ~16, ~274).

## Now

### Failed policy files are skipped, not fatal

`apply_policies` logs a failing file to `PolicyReport.failed` and continues, so
a deploy can report success with RLS only partially applied. Worth deciding
whether it should refuse like `ensure_fully_parsed` does for DDL. The scope
filter below removed the *expected* failures that made this backstop noisy, so
a remaining `failed` entry is now much more likely to be a real one.

### Ordering within `policies/` is alphabetical

Not currently a problem, and deliberately left alone: policies run after all
DDL, so every object a policy body can reference already exists, and no policy
file creates an object another one needs.

Recorded because it was once filed as the cause of a live failure and was not.
That report — a policy on `repository_metrics` referencing `team_projects` —
turned out to be a scope bug: `policies/` ignored `--scope`, so the file ran
against a plane with no `dojo` schema. The error named a **schema**, not a
missing table, and the DDL order was correct all along
(`dojo.can_read_repository_metric` carries 6 edges including
`dojo.team_projects` and lands a layer after it). Fixed by scope-filtering
`policies/`; see [the commands reference](guide/04-commands.md#policies-under---scope).

If a policy body ever does need another policy's object, `pg_query` already
extracts body references (`parser::pg::common`) and the files could be ordered
topologically like DDL entities.

## Future

### Convex

- Per-table `export_data` via the Convex CLI — currently errors with a clear
  message because `npx convex export` only supports whole-deployment dumps.
  Revisit if the CLI grows a `--table` flag, or extract the table from the
  export zip.
