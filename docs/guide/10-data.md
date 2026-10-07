# Data: import and export

`dbd import` loads data files into staging tables and then runs the procedures
that move the rows into place; `dbd export` writes table data back out to
files. Both are driven by folder conventions, so most projects need no
configuration at all.

## The `import/` convention

```
import/
  staging/
    lookups.csv               → staging.lookups, every environment
    lookup_values.jsonl       → staging.lookup_values, every environment
  dev/
    staging/
      fixtures.csv            → staging.fixtures, only under -e dev
```

A file at `import/<schema>/<table>.<ext>` loads into `<schema>.<table>`. The
extension picks the reader:

| Extension | Read as |
|---|---|
| `.csv` | CSV with a header row. On PostgreSQL the header is skipped and columns load **by position**, in the table's column order; on SQLite the header names the columns |
| `.tsv` | The same, tab-separated |
| `.jsonl` | JSON Lines — one JSON object per line |
| `.json` | JSON — a JSON array of records, or JSON Lines; a file whose first character is `[` is an array. On Convex, an array only |

Anything else under `import/` — a `notes.txt`, a `README.md` — is ignored, and
so is anything outside `import/`: data under `data/` or `seeds/` is never
scanned.

On PostgreSQL, CSV and TSV stream through `COPY`. JSON and JSON Lines records
are staged one at a time as bound `jsonb` values and moved into the table by a
procedure dbd installs as `staging.import_jsonb_to_table` — the load installs it
itself, creating the `staging` schema if it does not exist, so a JSON import
works on a database no import has touched. On SQLite every format loads through
batched `INSERT`s. Data files are read as UTF-8; a UTF-16 CSV is not decoded.

### Environment folders

A file directly under `import/<schema>/` loads in every environment. A file one
level deeper, under `import/<env>/<schema>/`, loads only when the run's
environment is `<env>`:

```sh
dbd import            # -e prod is the default: import/prod/… loads, import/dev/… does not
dbd import -e dev     # import/dev/… loads as well as the shared files
```

The environment is `dev` or `prod`, so the folders are `import/dev/` and
`import/prod/` (`-e development` and `-e production` are accepted and select
those same folders). A run that leaves files out says so —
`3 data file(s) under import/dev/ skipped — this run's env is 'prod'` — so a
deploy that seeded nothing is never mistaken for one that seeded everything.

## What a run does

```sh
dbd import --dry-run
```
```
  import staging.lookups (csv) ← ./import/staging/lookups.csv

  call staging.import_lookups()
1 entities — no issues
```

In order:

1. **Truncate** every staging table in the plan (unless `truncate: false`) —
   `TRUNCATE` on PostgreSQL, `DELETE FROM` on SQLite. On Convex the load itself
   replaces the table.
2. **Load** each data file into its table.
3. **Call the import procedures.** A procedure that *reads* a staging table is
   that table's import procedure, found by analysing its body — which tables it
   reads and which it writes — not by its name. The plan is ordered by
   dependencies, so a procedure that fills `config.lookups` runs before one that
   fills `config.lookup_values`. Only a **procedure** is a loader — the step is
   `CALL`, which PostgreSQL refuses for a function — so a function is never run.
   A function that reads a staging table no procedure loads is usually a loader
   written as a function, and the run names it in a warning: make it a
   procedure, or run it from `import.after`.
4. **Run `import.after`** scripts.

Dynamic SQL (`EXECUTE format(…)`) is invisible to that analysis; a procedure
whose staging read happens only inside dynamic SQL is not matched.

### `import.after`

Scripts listed under `import.after` run once the data is loaded and the
procedures have run. They run on **`dbd import` and `dbd deploy`** — the two
commands that run the import phase — and never on `dbd apply`, which does not
import. Post-DDL setup that should happen whenever the schema is applied
(publications, grants) belongs in `apply.after` instead. See
[Hook scripts](03-design-yaml.md#hook-scripts).

## Options

```yaml
import:
  options:
    truncate: true         # default: empty each staging table before loading it
    null_value: ''         # default: an empty CSV cell is NULL
  tables:
    - staging.legacy_export:
        null_value: '\N'   # this table's files mark NULL as \N
        truncate: false
        format: tsv        # parse as TSV whatever the extension says
  after:
    - import/loader.sql
```

- **`null_value`** is the string that means SQL NULL in a CSV or TSV cell. The
  default `''` makes an empty cell NULL. Set a sentinel such as `\N` when your
  files must tell an empty string from NULL: then only the sentinel is NULL and
  an empty cell is an empty string.
- **`tables:`** holds per-table overrides of `truncate`, `null_value` and
  `format`. It is not a list of what to import — every data file under
  `import/` loads whether or not its table is listed.
- **`truncate`** (default `true`) empties each staging table before it loads:
  `TRUNCATE` on PostgreSQL, `DELETE FROM` on SQLite, which has no `TRUNCATE`.
  Convex imports with `npx convex import --replace`, which always replaces the
  table, so there `truncate: false` has no effect (see [targets](08-targets.md)).

Three keys are parsed but not read, so they change nothing:
`import.staging`, `import.options.format` (the format comes from the file
extension, or a per-table `format`), and a per-table `env` (the environment is
the folder).

## Loading one file

```sh
dbd import -n staging.lookups                                # one table, from import/
dbd import -n activity.hook_events -f ~/data/events.jsonl    # one table, from any file
```

`-n` narrows the run to one table but otherwise runs it as usual —
`import.after` included. `-f` loads exactly the file you name into the `-n`
table, and nothing else: no truncate, no procedures, no `import.after`. Its
format comes from the extension — `.jsonl`, `.json` (an array of records or JSON
Lines), `.tsv`, and anything else is read as CSV. (On `export`, `-f` is the
*format*.)

Under `--scope`, a table the design declares must be one the scope builds —
otherwise `-f` refuses with `X is outside scope 'S'`. A table the design does not
declare is an ad-hoc target that no scope can contain, so it loads, with a
warning that the scope was not applied to it.

## Nothing is skipped silently

The summary always states the table count, including `0`, followed by the
reason for anything left out:

| Reason | Warning |
| --- | --- |
| No `import/` directory | `no import/ directory — nothing to import` |
| File belongs to another environment | `N data file(s) under import/<env>/ skipped — this run's env is '<env>'` |
| A file failed to parse | `staging table X not imported — data file failed to parse` |
| Entry cut by `--scope` | `X not imported — T is outside scope '<name>'` |
| `-n` matched no data file | `no staging data file matches --name <name>` |
| A function reads a staging table no procedure loads | `X has no loader procedure — function F reads it but is not run` |

Under `--scope`, an entry runs only where both of its halves can: every table
its procedure writes must be in the scope, and so must its staging table when
the design declares one (a staging table with no DDL of its own is not the
scope's to judge). An entry with no procedure needs only its own table. The
warning names whichever table is outside — the staging table or a target — and
`import --dry-run` and `deploy --dry-run` print the same lines the real run
does.

## Export

```sh
dbd export                                # every table, or what export: lists
dbd export -f jsonl                       # another format: csv (default), tsv, jsonl
dbd export -n config.lookups -f tsv       # one table
dbd export -o import/staging              # flat into a folder: import/staging/<table>.<fmt>
dbd export --scope hub                    # only what the scope builds
```

What is written:

- **No `export:` list** — every table in the design (within the scope).
- **An `export:` list** — only what it names: tables, and views or materialized
  views. `-n` then narrows that list.
- **A `-n` that selects nothing is an error**, not an empty export:
  `X is not listed under export: in design.yaml, so it is not exported`, or,
  without a list, `X is not a table — without an export: list, only tables are
  exported`. A name the design does not have is `no entity named 'X'`, and one
  the scope leaves out `X is outside scope 'S'`.
- **An entry that names nothing exportable is reported** —
  `export: X is not a table or view in this design — nothing exported for it` —
  and the rest of the list still exports.

```yaml
export:
  - config.lookups
  - config.lookup_values:
      format: jsonl        # this table always exports as JSON Lines
```

A per-table `format` wins over `-f`. The formats are `csv`, `tsv` and `jsonl`;
any other is refused (`cannot export X as json: Postgres exports csv, tsv or
jsonl`) rather than written as CSV under another name. Files land at
`export/<schema>/<table>.<fmt>` (on SQLite, `export/<table>.<fmt>`) — relative to
the current directory, not to `--source`; with `-o <dir>` they land flat in that
directory as `<dir>/<table>.<fmt>`. CSV and TSV carry a header row.

**A listed view exports like a table** — the rows its query returns, in its
column order. Unlisted, the default stays every table. The next section is what
that is for.

Convex refuses `export`: its CLI dumps whole deployments only (`npx convex
export`).

## Round trips

What `export` writes — CSV, TSV or JSON Lines — `import` reads back. Exporting
straight into the import tree makes the live data the seed data:

```sh
dbd export -n staging.lookups -o import/staging
# writes import/staging/lookups.csv — the file staging.lookups is loaded from
```

`-o` writes `<table>.<fmt>` without the schema, so point it at the folder of
the schema the file should load into.

A JSON Lines export round-trips too: each row is written as the JSON Postgres
renders for it, so a value holding a quote, a backslash or a newline loads back
as it was.

### Moving a long-lived system to a new database

A database that has run for years has data its seed files never knew about:
rows edited by hand, by the application, by support. To move such a system to a
new database with dbd, the import side is already the right shape — staging
tables that carry natural keys, and import procedures that resolve them into
foreign keys. What is needed is an export in that same shape.

The pattern is a set of views that dereference each foreign key back to the
natural key the staging table expects — `lookup_values` joined to `lookups` so
each row carries the lookup's *name* rather than its id — listed under
`export:` so that exporting from the old database produces files that match the
staging tables column for column. Importing those files into the new database
then runs the same procedures a fresh seed does.

`-o` names each file after the view, without its schema, so name each view
after the staging table it feeds, and keep its columns in that table's order —
on PostgreSQL a CSV loads by position:

```yaml
export:
  - staging_export.lookups
  - staging_export.lookup_values
```

```sh
dbd export -d $OLD_DATABASE_URL -o import/staging   # import/staging/lookups.csv, lookup_values.csv
dbd deploy -d $NEW_DATABASE_URL
```

None of this is needed when the import files are the well-managed source of
truth — when every change to reference data goes through them.
