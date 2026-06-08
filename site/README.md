# dbd website

Marketing + docs site for **dbd**, built from the design handoff in
[`../docs/mockup`](../docs/mockup).

## Stack

- **SvelteKit** (Svelte 5 runes) + **Vite**, fully prerendered (`adapter-auto`
  detects Vercel at build time — no explicit adapter config needed).
- **UnoCSS** via **Rokkit** (`@rokkit/unocss` `presetRokkit`) for the semantic
  token system. Roles/palettes are configured in [`rokkit.config.js`](./rokkit.config.js);
  components use the z-scale utilities (`bg-surface-z0`, `text-primary-z5`,
  `text-on-primary`, …) which flip automatically under `[data-mode="dark"]`.
  Component styling (the `Button`) uses the `@rokkit/themes` `zen-sumi` skin.
- Fonts via **Fontsource** (Space Grotesk / IBM Plex Sans / IBM Plex Mono).
- Package manager: **bun**.

## Content is synced from `/docs` — single source of truth

`scripts/copy-content.mjs` runs on `predev` and `prebuild`:

- `docs/llms/*.txt` → `src/lib/content/llms/` → served by `+server.ts` routes
  at `/llms.txt` and `/llms-full.txt` with `text/plain; charset=utf-8` (the
  explicit charset keeps the UTF-8 em-dashes from rendering as mojibake)
- `docs/guide/*.md` → `src/lib/content/guide/` → rendered at `/guide/<slug>`
  (markdown via `marked`)

Synced outputs are gitignored; edit the originals under `/docs`.

## Commands

```sh
bun install
bun run dev          # local dev (syncs content first)
bun run build        # production build (prerenders every route)
bun run preview      # preview the production build
bun run sync:content # re-sync docs → site manually
```

## Routes

- `/` — landing page (hero, overview, concepts, targets, audience, get-started)
- `/guide` and `/guide/<slug>` — the user guide (from `docs/guide`)
- `/llms.txt`, `/llms-full.txt` — LLM reference (from `docs/llms`)

## Note on `bun run check`

`svelte-check` reports type errors inside `node_modules/@rokkit/ui` — that
package ships source `.ts` that imports the source-only `@rokkit/states`
(no bundled declarations), which is an upstream packaging gap, not a defect in
this app's code. `bun run build` is the source of truth for correctness and is
green.
