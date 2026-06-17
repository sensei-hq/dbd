# Ad-hoc import file + export output dir — design

- **Date:** 2026-06-17
- **Status:** approved (design)
- **Target release:** patch v0.7.3

## Goal

Let `import`/`export` work with explicit paths instead of only the `import/`/`export/`
folder conventions:

- `dbd import -n <entity> -f <file>` — load a specific file into a table.
- `dbd export -n <entity> -f <fmt> -o <dir>` — export a table to a chosen directory.

Both new options are **optional**; omitting them preserves today's convention-based behavior.

## CLI changes

### `import`
- Add `#[arg(short = 'f', long = "file")] file: Option<PathBuf>` to the `Import` variant
  (alongside the existing `--name`/`-n` and `--dry-run`).
- Semantics:
  - `-f` omitted → unchanged: import from the convention (`import/<schema>/<name>.<ext>`,
    all tables when `-n` is also omitted).
  - `-f <path>` given → **requires `-n <entity>`**; import that single file into the entity.
    The **format is inferred from the file extension** (`.jsonl`→jsonl, `.tsv`→tsv,
    `.csv`/other→csv). `--dry-run` prints `import <entity> ← <path>` and writes nothing.
- (import has no `-f` today, so this is purely additive.)

### `export`
- Keep `-f/--format` **unchanged** (csv/tsv/jsonl, default csv) — no breaking change.
- Add `#[arg(short = 'o', long = "output")] output: Option<PathBuf>` to the `Export` variant:
  the destination **directory**.
  - `-o` omitted → unchanged: `export/<schema>/<name>.<fmt>`.
  - `-o <dir>` given → write `<dir>/<name>.<fmt>` (flat — `<dir>` is the target directory;
    no `<schema>` subdir is added, since the caller chose the dir). `<name>` is the bare
    table name. `-o` requires `-n` (a single table) to avoid surprising multi-file writes
    into one dir with ambiguous names — actually multiple tables into one dir is fine
    (distinct names), so `-o` works with the all-tables form too; just document that all
    selected tables land flat in `<dir>`.

## Adapter changes

### `export_data` — accept an optional output directory
`export_data(&self, entity: &Entity)` currently hardcodes `Path::new("export").join(schema)`.
Change the trait + all impls (postgres, sqlite, convex) to
`export_data(&self, entity: &Entity, out_dir: Option<&Path>)`:
- `Some(dir)` → write `dir.join(format!("{name}.{format}"))` (create `dir`).
- `None` → today's `export/<schema>/<name>.<format>`.
`name` is the bare table name (strip the `schema.` prefix), `format` from `entity.format`.

### Ad-hoc import — reuse `import_data`
`import_data(&self, entity, dry_run)` already reads from `entity.file` + `entity.format`. The
CLI builds an `Entity` for the resolved `<entity>` (so the COPY target name/schema is correct),
sets `entity.file = Some(<path>)` and `entity.format = Some(<ext-derived>)`, and calls
`import_data`. No adapter change needed for import.

## CLI handler changes (`commands/data.rs` + dispatch in `commands/mod.rs`)

- `cmd_import`: when `file` is `Some`, resolve `<entity>` against the design (to get the
  schema-qualified name), build the one entity with `file`/`format` set, and import just that
  (honoring `--dry-run`). When `file` is `None`, the existing folder-convention path runs.
  Error clearly if `-f` is given without `-n`.
- `cmd_export`/`cmd_export_dry_run`(if any): thread `output: Option<&Path>` into
  `export_data(..., output)`. The per-table effective format logic is unchanged.
- Dispatch: pass the new `file`/`output` args through.

## Testing

- **Adapter unit (export out_dir)** — export a table with `out_dir = Some(tmp)` → file lands at
  `tmp/<name>.<fmt>`; with `None` → `export/<schema>/<name>.<fmt>` (existing behavior). (Use the
  embedded-pg or sqlite test harness; assert the file path + contents.)
- **format-from-extension helper** — a pure `fn format_from_ext(path) -> &str`
  (`.jsonl`→jsonl, `.tsv`→tsv, else csv), unit-tested.
- **CLI parse tests** — `dbd import -n t -f f.jsonl`, `dbd export -n t -f jsonl -o out/` parse;
  back-compat (`dbd import`, `dbd import -n t`, `dbd export -n t -f jsonl`) still parse.
- **e2e (manual)** — round-trip: `dbd export -n <t> -f jsonl -o import/staging` →
  `import/staging/<t>.jsonl`; then `dbd import -n <t> -f import/staging/<t>.jsonl` loads it.

## Docs (all three surfaces)

- `docs/guide/04-commands.md` — `import` `-f/--file` + `export` `-o/--output`, with the
  round-trip example; note `-f` is file on import / format on export.
- `docs/llms/llms.txt` + `llms-full.txt` — the new flags in the `dbd import`/`dbd export` notes.
- (No diagram change needed.)

## Out of scope

- Changing export's `-f` from format (kept for non-breaking).
- Reading import files from arbitrary remote/URLs (local files only).
