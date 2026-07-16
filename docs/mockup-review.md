# dbd website — mockup review

Review of the landing-page mockup (`docs/mockup/data.js`) against the **actual shipped product** (v0.8.7). For Claude Designer. Fixes flagged "DONE" are already applied to the live site; everything else is for the designer.

## Current state (context for the designer)

- The live site lives in `site/`. All landing-page copy is data-driven from **`site/src/lib/data.ts`** (mirrors the mockup `data.js`). The page `site/src/routes/+page.svelte` renders these exported blocks in order:
  - `hero` → `overview` (6 feature cards) → `concepts` (4 items, each has a code block) → `targets` (4 cards) → `audience` (4 cards) → `start` (3 steps).
- Content model / exports to plug into: `brand`, `nav`, `hero`, `overview.features[]`, `concepts.items[]`, `targets.items[]`, `audience.items[]`, `start.steps[]`, `footer`.
- Card shapes already in use:
  - `overview.features[]` = `{ tag, title, body }`
  - `concepts.items[]` = `{ id, kicker, title, body, code: { lang, label, source } }`
  - `targets.items[]` = `{ name, scheme, body, notes[] }`
- The site already has extra routes the landing page barely references: **`/diagram`** and **`/projects`** (the hosted interactive schema viewer) and **`/guide`**.

## The gap in one line

The product ships ~20 CLI commands; the landing page markets ~6. Two headline features (reverse-engineering an existing DB, and the interactive diagram viewer) are invisible, and the "ship it anywhere" promise (GitHub deploy) is never shown.

---

## 1. Factual fixes

- **DONE** — Install command was `cargo install dbd` (wrong crate → fails). Corrected to `cargo install dbd-cli` in `hero.install` and `start.steps[0]`.
- **DONE** — Scaffold command was `dbd init my-project` (invalid; `name` is a `--name`/`-n` flag, not positional). Corrected to `dbd init --name my-project`.
- **TODO (design decision)** — `targets.items[1].scheme` is `"supabase"`, which reads like a URL scheme. There is **no `supabase://` scheme**. Supabase is a normal `postgres://…` connection with `target: supabase` in `design.yaml`. Update the card so the "scheme" slot shows something honest, e.g. `postgres:// + target: supabase`, or relabel the slot from "scheme" to "connection".
- **Note** — the mockup `data.js` still has placeholder links (`repo: "https://github.com"`, footer "DBML docs" → `#`). The live `data.ts` already uses the real repo (`https://github.com/sensei-hq/dbd`) and real footer links. Use the live values, not the mockup placeholders.

---

## 2. Missing features to add (with ready-to-use data points)

Priority order. Each block is drop-in copy for the existing card shapes.

### High priority

- **Reverse-engineer an existing database** (biggest missing onboarding hook). Suggest a new hero-adjacent section or a prominent concept item.
  - Pitch: "Already have a database? Adopt it in one command."
  - `dbd init --from-db $DATABASE_URL` — turn a live Postgres/Supabase into a managed dbd project (DDL files + design.yaml).
  - `dbd init --from-dbml schema.dbml` — bootstrap from a DBML file instead.
  - `dbd merge $DATABASE_URL` — sync an existing/changed DB back into the current project (reverse-engineer + reconcile).
  - Data point for an overview card: `{ tag, title: 'Adopt an existing DB', body: 'Reverse-engineer a live Postgres, Supabase, or a DBML file into a managed dbd project with one command — dbd init --from-db.' }`

- **Interactive schema diagram viewer** (the site already hosts it at `/diagram` + `/projects`, but the copy never mentions it — only DBML export).
  - `dbd diagram` — open the schema in a hosted interactive viewer (`--print-url` for the link, `--json` for the raw model).
  - Update `overview.features[3]` ("Documentation") to cover both: DBML export **and** the interactive viewer, or add a dedicated card + link to `/diagram`.
  - Data point: `{ tag, title: 'Visual schema viewer', body: 'dbd diagram opens your schema in a hosted, interactive diagram — no DBML round-trip needed.' }`

- **Deploy from GitHub** (delivers on the "ship it anywhere" tagline, currently unshown).
  - `dbd deploy --source org/repo/path@ref -d $DATABASE_URL` — fetch schema from a GitHub repo (or local path) + apply + import in one step.
  - Data point: `{ tag, title: 'Deploy from Git', body: 'dbd deploy pulls a schema straight from a GitHub repo (pinned to a tag) and applies it — CI-friendly, no checkout needed.' }`

### Medium priority

- **Two schema-evolution modes** (page shows only snapshots). Consider expanding the `snapshots` concept item to mention the lifecycle:
  - Pre-release: `dbd reconcile` — diff the live DB against the design and apply ALTER/CREATE in place, no snapshots.
  - Lock in: `dbd release` (alias `baseline`) — write a baseline snapshot and switch to the versioned snapshot + migration workflow.
  - Also worth noting: "smart multi-snapshot" auto-splits risky changes (column rename, type change, enum-value removal) into safe staged migrations.

- **DDL formatter + pre-commit** (concrete DX selling point).
  - `dbd format` — river-style DDL formatter; `dbd format --check` exits non-zero for CI/pre-commit. Ships a `dbd-format` pre-commit hook.
  - Data point: `{ tag, title: 'Formatter + pre-commit', body: 'dbd format keeps DDL tidy (river-style); dbd format --check drops into pre-commit and CI.' }`

- **RLS policies** (notable given Supabase is a headline target).
  - `dbd policies` — apply row-level-security policies from `policies/`.
  - Data point: `{ tag, title: 'Row-level security', body: 'Manage Postgres/Supabase RLS policies as code in policies/ and apply them with dbd policies.' }`

- **Use as an embeddable library** (audience card already promises this — back it with a real feature/snippet).
  - Library crate `dbd-core`: `Design::from_config(path, "prod")?.apply(&adapter, None, false).await?`.
  - Consider a small "Use as a library" section with a Rust snippet + link to the guide.

### Lower priority (a "more commands" strip would cover these)

- `dbd inspect` — validate config + report unresolved references (works offline via `.dbd/refcache.json` cache).
- `dbd export` — dump table data to csv/tsv/jsonl (page only shows import).
- `dbd doctor` — audit/migrate design.yaml + DDL layout.
- `dbd reset` — drop project schemas with safety guards.
- `dbd combine` — combine all DDL into one SQL file.
- `dbd graph` — output the dependency graph as JSON.
- `dbd migrate --status` — show migration version status.

---

## 3. What's already accurate (leave as-is)

- DDL-as-source-of-truth + "file path is the entity name", the `design.yaml` example, snapshots → auto-generated migrations, the four targets and their adapter quirks (Postgres/Supabase/SQLite/Convex), dependency ordering, data-loading formats (CSV/TSV/JSONL), scoped deployments (overview card 06), and the audience framing all match the product.
- Both `.ddl` and `.sql` file extensions are valid (the mockup mixing them is fine; `.ddl` is canonical).
