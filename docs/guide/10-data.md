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
| `.csv` | CSV. The first line is a header and is skipped; columns load **by position**, in the table's column order |
| `.tsv` | The same, tab-separated |
| `.jsonl`, `.json` | JSON Lines — one JSON object per line (a `.json` file is read the same way) |

Anything else under `import/` — a `notes.txt`, a `README.md` — is ignored, and
so is anything outside `import/`: data under `data/` or `seeds/` is never
scanned.

On PostgreSQL, CSV and TSV stream through `COPY`; JSON Lines go through a
procedure dbd installs as `staging.import_jsonb_to_table`, creating the
`staging` schema if it does not exist.

### Environment folders

A file directly under `import/<schema>/` loads in every environment. A file one
level deeper, under `import/<env>/<schema>/`, loads only when the run's
environment is `<env>`:

```sh
dbd import            # -e prod is the default: import/prod/… loads, import/dev/… does not
dbd import -e dev     # import/dev/… loads as well as the shared files
```

The environment is `dev` or `prod` (`development` and `production` are accepted
and mean the same). A run that leaves files out says so —
`3 data file(s) under import/dev/ skipped — this run's env is 'prod'` — so a
deploy that seeded nothing is never mistaken for one that seeded everything.

## What a run does

```sh
dbd import --dry-run
```
```
  import staging.models (jsonl) ← import/staging/models.jsonl
  import staging.routers (jsonl) ← import/staging/routers.jsonl

  call staging.import_models()
  call staging.import_routers()
```

In order:

1. **Truncate** every staging table in the plan (unless `truncate: false`).
2. **Load** each data file into its table.
3. **Call the import procedures.** A procedure or function that *reads* a
   staging table is that table's import procedure, found by analysing its body —
   which tables it reads and which it writes — not by its name. The plan is
   ordered by dependencies, so a procedure that fills `config.lookups` runs
   before one that fills `config.lookup_values`.
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

Three keys are parsed but not read in 0.24.1, so they change nothing:
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
table, with the format taken from its extension, and nothing else: no
truncate, no procedures, no `import.after`, no scope filtering. (On `export`, `-f` is the
*format*.)

## Nothing is skipped silently

The summary always states the table count, including `0`, followed by the
reason for anything left out:

| Reason | Warning |
| --- | --- |
| No `import/` directory | `no import/ directory — nothing to import` |
| File belongs to another environment | `N data file(s) under import/<env>/ skipped — this run's env is '<env>'` |
| A file failed to parse | `staging table X not imported — data file failed to parse` |
| Entry cut by `--scope` | `staging table X not imported — outside scope '<name>'` |
| `-n` matched no data file | `no staging data file matches --name <name>` |

Under `--scope`, an entry is dropped when its procedure writes to any table
outside the scope — or, with no procedure, when the staging table itself is
outside it.

## Export

```sh
dbd export                                # every table, or the ones export: lists
dbd export -f jsonl                       # another format: csv (default), tsv, jsonl
dbd export -n config.lookups -f tsv       # one table
dbd export -o import/staging              # flat into a folder: import/staging/<table>.<fmt>
dbd export --scope hub                    # only the scope's tables
```

Which tables are written:

- **No `export:` list** — every table in the design (within the scope).
- **An `export:` list** — only the tables it names. `-n` then narrows that list,
  so `-n` naming a table the list leaves out prints `No tables to export.`

```yaml
export:
  - config.lookups
  - config.lookup_values:
      format: jsonl        # this table always exports as JSON Lines
```

A per-table `format` wins over `-f`. Files land at
`export/<schema>/<table>.<fmt>`; with `-o <dir>` they land flat in that
directory as `<dir>/<table>.<fmt>`. CSV and TSV carry a header row.

**Export writes tables only.** A view named under `export:` is skipped without a
message in 0.24.1 — see the next section for what that means for moving data.

Convex refuses `export`: its CLI dumps whole deployments only (`npx convex
export`).

## Round trips

What `export` writes, `import` reads. Exporting straight into the import tree
makes the live data the seed data:

```sh
dbd export -n staging.lookups -o import/staging
# writes import/staging/lookups.csv — the file staging.lookups is loaded from
```

`-o` writes `<table>.<fmt>` without the schema, so point it at the folder of
the schema the file should load into.

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

In 0.24.1 that last step cannot go through `dbd export`, which writes tables
only and skips the views. Until it can, export each view with `psql` into the
file its staging table loads from — CSV with a header row, columns in the
staging table's order:

```sh
psql "$OLD_DATABASE_URL" -c "\copy (select * from staging_export.lookup_values) \
  to 'import/staging/lookup_values.csv' with (format csv, header)"
dbd deploy -d $NEW_DATABASE_URL
```

None of this is needed when the import files are the well-managed source of
truth — when every change to reference data goes through them.
