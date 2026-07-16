# dbd website — mockup review

Review of the landing-page mockup against the **actual shipped product** (v0.8.7). For Claude Designer. Updated for the current mockup revision.

## Status: no open items

The current mockup is accurate against the product. Every item from earlier rounds is resolved.

## Current state (context for the designer)

- **Current mockup:** `docs/mockup/dbd.dc.html` — single self-contained file. All copy lives inline in the `DBD_DATA` object (~lines 260–360): `hero`, `overview.features[]`, `concepts.items[]`, `targets.items[]`, `commands.items[]`, `audience.items[]`, `start.steps[]`, `footer`.
- **Live site** content is data-driven from `site/src/lib/data.ts` (rendered by `site/src/routes/+page.svelte`) and is in sync with the mockup — all 12 overview cards, the 5 concepts, the 8-command toolbelt (incl. `dbd diagram`), and the latest copy edits are ported.
- Card shapes: `overview.features[]` = `{ tag, title, body }`; `concepts.items[]` = `{ id, kicker, title, body, code }`; `targets.items[]` = `{ name, scheme, body, notes[] }`; `commands.items[]` = `{ cmd, body }`.

---

## Resolved (✓ done — no action)

- **Factual fixes:** `cargo install dbd` → `cargo install dbd-cli`; `dbd init my-project` → `dbd init --name my-project`. Both correct in the mockup and live site.
- **Supabase card** `scheme` → `"postgres:// + target: supabase"`; adapters code block uses `target: supabase`.
- **Added overview cards:** Formatter + pre-commit (07), Row-level security (08), Reverse-engineer a database (09), Interactive schema diagram / `dbd diagram` (10), Deploy straight from GitHub / `dbd deploy` (11).
- **Added concepts:** "Two modes of schema evolution" (reconcile → release/baseline), "Use it as a library" (`dbd-core` + Rust snippet).
- **Added the "full toolbelt" section:** `inspect`, `export`, `doctor`, `reset`, `combine`, `graph`, `migrate --status`.
- **Environment-scoped data** overview card (12) — `import/<env>/<schema>/` loads only under `-e <env>`.

---

## Accurate — leave as-is

- DDL-as-source-of-truth + "file path is the entity name", the `design.yaml` example, the four targets and their adapter quirks, dependency ordering, data-loading formats (CSV/TSV/JSONL), scoped deployments, and the audience framing all match the product.
- Both `.ddl` and `.sql` file extensions are valid (mixing them in the example is fine; `.ddl` is canonical).
- The site already hosts the interactive viewer at `/diagram` + `/projects` — the new "Interactive schema diagram" card can link there.
