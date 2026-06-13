# Schema Diagram Viewer v2 — hosted viewer + CLI deep-link (design)

**Status:** approved-pending-review
**Date:** 2026-06-12
**Supersedes (partially):** v1 local self-contained HTML (`dbd diagram` → `schema.html`, shipped v0.4.9). v2 **removes** the embedded-HTML path in favor of the hosted site.

## Goal

`dbd diagram` builds the schema model and **opens the hosted dbd site** (`https://dbd-sigma.vercel.app/diagram`) with the model embedded in the URL fragment, rendering the same interactive viewer — but always on the latest, consistently-tokened Rokkit build, with no 652 KB bundle to embed or keep fresh. A `/diagram` route on the site renders a model from either the URL fragment (CLI deep-link / shareable link) or a manually uploaded `schema.json`. Everything is client-side; the site stays static.

## Non-goals (deferred)

- **Auth / accounts** — v3 (kavach + Supabase).
- **Server-side storage / versioning / short links** — v3.
- **UI sharing of stored diagrams** — v4.
- **Views / functions / procedures** as diagram nodes — later (model already extensible).
- Reading a `file://` path from the hosted page — impossible (browser blocks hosted origins from the local filesystem); the fragment-embed achieves the same goal instead.

## Why drop the local HTML

The v1 self-contained HTML embedded the whole Svelte+Rokkit viewer (with base64 fonts) into every output file via `include_str!`, requiring a committed `diagram_viewer.js` lib bundle, a `make viewer` build step, and a CI freshness check. Routing through the hosted site instead means: one always-current viewer, the full Rokkit token ecosystem available and consistent, no bundle drift, and a much smaller Rust crate. **Trade-off accepted:** viewing now requires the hosted site to be reachable (no offline self-contained file). `dbd diagram --json` still emits the raw model offline.

## Architecture

```
dbd diagram ──build SchemaModel──▶ gzip + base64url ──▶ https://dbd-sigma.vercel.app/diagram#1.<payload>
                                                              │ (fragment is client-only,
                                                              │  never sent to the server)
                                                              ▼
                              static /diagram route ──decode──▶ validate ──▶ <Viewer model>
                                          ▲
                              file upload / drag-drop of schema.json ──┘
```

Two cooperating units with one shared contract (the fragment encoding):
- **Rust** (`dbd-core` encode + `dbd-cli` command): build model → encode → assemble URL → open browser / print.
- **Site** (`/diagram` route + `fragment.ts`): decode fragment OR read uploaded file → validate → render `Viewer`.

## Encoding contract (CLI ↔ site)

The single interface between the two units. **Stable, versioned:**

- Payload = `serde_json::to_vec(model)` → **gzip** (flate2, default compression) → **base64url, no padding** (`base64::engine::general_purpose::URL_SAFE_NO_PAD`).
- Fragment = `#` + `"1."` + payload. The leading `1.` is a format version for forward-compat; the site splits on the first `.`.
- Full URL = `{site}/diagram#1.{payload}` where `{site}` has no trailing slash.

**Browser decode** (`fragment.ts`): strip `#`, split version off the first `.`, reject unknown versions; base64url-decode → `Uint8Array` → `new Response(blob.stream().pipeThrough(new DecompressionStream('gzip')))` → text → `JSON.parse`. If `DecompressionStream` is unavailable, fall back to `fflate.gunzipSync` (tiny dep, added to the site). Round-trips losslessly with the Rust encoder.

## Command surface (`dbd diagram`)

| Invocation | Behavior |
|---|---|
| `dbd diagram` | Build model → encode → open `{site}/diagram#1.…` in the default browser **and** print the URL. |
| `dbd diagram --json [-f schema.json]` | Write the model JSON to a file (default `schema.json`); no browser. For manual upload, tooling/CI, and v3. |
| `dbd diagram --print-url` | Print the URL only; don't open the browser (headless/CI/remote/copy-paste). |
| `dbd diagram --site <url>` | Override the base URL for this run. |
| env `DBD_DIAGRAM_URL` | Override the base URL (flag wins over env wins over default). |

- Default base URL constant: `https://dbd-sigma.vercel.app` (one place to change; documented as provisional).
- Scope-aware (`--scope` / `--deps`) exactly as today.
- **Headless fallback:** if opening the browser fails (no display / `open` errors), print the URL with a short note and exit `0`.
- **Oversized warning:** if the encoded payload exceeds ~1.5 MB (pathologically large schema), print a warning suggesting `--json` + manual upload, but still emit the URL.
- `--json` and the browser default are mutually exclusive in effect: `--json` writes a file and never opens a browser.

## Site `/diagram` route

A prerendered SvelteKit route (`+page.svelte` + `+page.ts` with `export const prerender = true; export const ssr = false;` — it's a client-only SPA page; the fragment isn't available during SSR anyway). It resolves a model from, in priority order:

1. **URL fragment** `#1.<payload>` present → `decodeFragment` → validate → render (CLI deep-link / shareable link). On decode/validation failure: show a friendly error panel with the reason and the upload affordance.
2. **File upload / drag-drop** of a `.json` file → `FileReader.readAsText` → `JSON.parse` → validate → render. Replaces fragment content in-view (does not rewrite the URL).
3. **Empty state** (no fragment, no file) → a centered dropzone, a "Choose file" button, and **"Load example"** which renders a small bundled sample model so first-time visitors see the viewer immediately.

**Validation** (`validateModel` in `model.ts`): assert top-level shape — `project.name`, arrays `schemas` / `tables` / `refs`, and tables having `schema`/`name`/`columns`. Returns a typed result (`{ok:true,model}` | `{ok:false,error}`); the route renders the error string. Guards against arbitrary/old/corrupt JSON.

The route reuses the existing `Viewer` component unchanged. Header/chrome from `Viewer` already covers project name, counts, theme toggle, density/arrange.

## Removals (cleanup of the v1 embedded path)

Delete:
- `crates/dbd-core/assets/diagram.html`
- `crates/dbd-core/assets/diagram_viewer.js` (the committed 652 KB bundle)
- `crates/dbd-core/src/diagram.rs::render_html` and its two `include_str!`s (replace the file's contents with the encoder; keep `pub mod diagram`).
- `site/vite.viewer.config.ts`
- `site/package.json` script `build:viewer`; dev-dep `vite-plugin-css-injected-by-js`.
- The base64 `@font-face` block in `site/src/lib/viewer/viewer.css` (bundle-only; the hosted site loads the three faces via its normal `@fontsource` imports — verify `+layout`/`app.css` imports Space Grotesk + IBM Plex Sans/Mono, add the imports if missing).
- `Makefile` `viewer` target.
- The CI **viewer-bundle freshness** job/step in `.github/workflows/ci.yml`.
- `site/src/lib/viewer/index.ts` `mountViewer` export if unused after the route lands (the route imports `Viewer` directly). Keep `Viewer`/types exports.

## File structure

**Rust**
- Modify `crates/dbd-core/src/diagram.rs` → `encode_payload(&SchemaModel) -> Result<String>` (gzip+base64url, no `1.` prefix) and `fragment_url(base: &str, model: &SchemaModel) -> Result<String>` (assembles `{base}/diagram#1.{payload}`). Remove `render_html`.
- `crates/dbd-core/Cargo.toml` → add `flate2`, `base64`.
- Modify `crates/dbd-cli/src/commands/diagram.rs` → new control flow (json → write file; else build URL, print, open unless `--print-url`); browser open via the `open` crate; oversized + headless handling.
- `crates/dbd-cli/Cargo.toml` → add `open`.
- Modify `crates/dbd-cli/src/cli.rs` → `Diagram { json, file, print_url, site }`; reword help; keep `every_subcommand_parses` test.

**Site**
- Create `site/src/routes/diagram/+page.svelte`, `site/src/routes/diagram/+page.ts`.
- Create `site/src/lib/viewer/fragment.ts` (`decodeFragment(hash): Promise<unknown>` + encode helper for tests/symmetry).
- Create `site/src/lib/viewer/sample.ts` (bundled example model) — small, hand-written, a couple schemas + refs.
- Modify `site/src/lib/viewer/model.ts` → add `validateModel(value): {ok:true,model:SchemaModel}|{ok:false,error:string}`.
- Modify `viewer.css` (remove base64 fonts), `package.json`, `Makefile`, CI, docs.
- Add dep `fflate` (gzip fallback for browsers lacking `DecompressionStream`, e.g. Safari < 16.4). Primary path is native `DecompressionStream('gzip')`; `fflate.gunzipSync` is the fallback. Both paths covered by the `decodeFragment` test.

**Docs**
- `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`, `README.md` — `dbd diagram` opens the hosted interactive viewer; `--json` emits the model; `--print-url`/`--site` documented; remove "self-contained HTML" wording.

## Error handling

- **CLI:** model build / scope errors as today (`?` + context). Browser-open failure → print URL + note, exit 0. Encoding failure → hard error.
- **Site:** decode failure (bad base64 / bad gzip / unknown version) and validation failure → friendly inline error + upload affordance, never a blank screen or a thrown uncaught error. Oversized fragment that the browser truncated → surfaces as a decode/validate error with a hint to use file upload.

## Testing

- **Rust:** `encode_payload` round-trip (gzip+base64url decodes back to the same model bytes); `fragment_url` shape (`/diagram#1.`); `--print-url` prints a URL and writes no file; `--json -f` writes the file and opens nothing; `diagram` parses in `every_subcommand_parses`.
- **Site (vitest):** `decodeFragment` round-trips a model encoded by a JS encoder mirroring the Rust one and rejects junk/unknown-version; `validateModel` accepts a good model and rejects malformed ones; a `@testing-library/svelte` test mounting `/diagram` `+page` with a fragment renders cards, and with an uploaded file renders cards.
- **Manual smoke:** `dbd diagram --print-url` from sensei → paste into the running dev site (`--site http://localhost:5173`) → renders; `dbd diagram` opens the browser.

## Done when

`dbd diagram` opens `https://dbd-sigma.vercel.app/diagram#1.…` showing the interactive viewer for the current project (browser opened + URL printed); `--print-url` prints it without opening; `--json` still writes the model file; the `/diagram` route renders from a fragment, an uploaded file, or the bundled example, with friendly errors on bad input; the local-HTML path, lib-bundle build, and CI freshness check are removed; `cargo test --workspace` + clippy green; `bun run test:viewer` green. Then release.
