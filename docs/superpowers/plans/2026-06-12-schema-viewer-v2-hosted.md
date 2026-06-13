# Schema Viewer v2 — Hosted Viewer + CLI Deep-Link Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. For every `.svelte`/`.svelte.ts` file use the **svelte (svelte-file-editor / Svelte MCP)** skill, and **semantic-styles-rokkit** for tokens. Steps use checkbox (`- [ ]`).

**Goal:** `dbd diagram` builds the schema model, gzip+base64url-encodes it into a URL fragment, and opens the hosted dbd site (`/diagram`) to render the interactive viewer; the `/diagram` route also accepts a manually uploaded `schema.json`. The embedded local-HTML path, the lib-bundle build, and the CI freshness check are removed.

**Architecture:** Two units sharing one encoding contract. **Rust** (`dbd-core::diagram` encode + `dbd-cli` command) builds the model → gzip → base64url → `{site}/diagram#1.{payload}` → opens the browser / prints the URL. **Site** (`/diagram` SvelteKit route + `fragment.ts`) decodes the fragment OR reads an uploaded file → validates → renders the existing `Viewer`. Fully client-side; the site stays static (prerendered, Vercel + Cloudflare Pages via adapter-auto).

**Tech Stack:** Rust (`flate2` gzip, `base64` URL_SAFE_NO_PAD, `open` to launch the browser); SvelteKit 2 + Svelte 5 runes; native `CompressionStream`/`DecompressionStream` with `fflate` fallback; vitest. Spec: `docs/superpowers/specs/2026-06-12-schema-viewer-v2-hosted-design.md`.

**Release gate:** This is what makes `dbd diagram` hosted-viewer-based; `make bump` after it lands and the `/diagram` route is deployed.

---

## Contract: the URL fragment (CLI ↔ site)

`serde_json(model)` → **gzip** → **base64url, no padding** → fragment `#1.<payload>` (leading `1.` = format version). The full URL is `{base}/diagram#1.<payload>` where `{base}` has no trailing slash. Default base = `https://dbd.sensei-hq.com` (overridable). Fragments are client-only (never sent to the server) → schema stays private, no server URL limits. The browser decodes with native `DecompressionStream('gzip')`, falling back to `fflate.gunzipSync`.

## File structure

**Rust**
- Modify `crates/dbd-core/src/diagram.rs` — replace `render_html` with `encode_payload` + `fragment_url`.
- Modify `crates/dbd-core/Cargo.toml` — `flate2` non-optional, add `base64`.
- Delete `crates/dbd-core/assets/diagram.html`, `crates/dbd-core/assets/diagram_viewer.js`.
- Modify `crates/dbd-cli/src/commands/diagram.rs` — new control flow + `resolve_site` helper.
- Modify `crates/dbd-cli/src/commands/mod.rs` — dispatch the new `Diagram` fields.
- Modify `crates/dbd-cli/src/cli.rs` — `Diagram { json, file, print_url, site }`.
- Modify `crates/dbd-cli/Cargo.toml` — add `open`.

**Site**
- Create `site/src/lib/viewer/fragment.ts` (+ `fragment.test.ts`).
- Create `site/src/lib/viewer/sample.ts`.
- Modify `site/src/lib/viewer/model.ts` — add `validateModel` (+ test in `model.test.ts`).
- Create `site/src/routes/diagram/+page.ts`, `site/src/routes/diagram/+page.svelte` (+ `diagram.page.test.ts`).
- Modify `site/src/routes/+layout.svelte` — hide Nav/Footer on `/diagram`.
- Modify `site/src/lib/viewer/Viewer.svelte` — drop standalone-only theme wiring.
- Modify `site/src/lib/viewer/index.ts` — drop `mountViewer`.
- Modify `site/src/lib/viewer/viewer.css` — drop base64 `@font-face` block.
- Modify `site/package.json` (`fflate` dep; remove `build:viewer` + `vite-plugin-css-injected-by-js`), delete `site/vite.viewer.config.ts`.
- Modify `Makefile` (remove `viewer`), `.github/workflows/ci.yml` (remove `viewer-bundle`).

**Docs:** `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`, `README.md`.

---

## Task 1: Rust encode (`dbd-core::diagram`)

**Files:**
- Modify: `crates/dbd-core/Cargo.toml`
- Modify (replace contents): `crates/dbd-core/src/diagram.rs`
- Delete: `crates/dbd-core/assets/diagram.html`, `crates/dbd-core/assets/diagram_viewer.js`

- [ ] **Step 1: deps.** In `crates/dbd-core/Cargo.toml`, make `flate2` non-optional and add `base64`. Change:
```toml
flate2 = { version = "1", optional = true }
```
to:
```toml
flate2 = "1"
base64 = "0.22"
```
and in `[features]` change `deploy = ["dep:reqwest", "dep:flate2", "dep:tar"]` to `deploy = ["dep:reqwest", "dep:tar"]` (flate2 is now always available).

- [ ] **Step 2: replace `diagram.rs`** with the encoder (this removes `render_html`, the `include_str!`s, and the HTML-escape helpers):
```rust
//! Encode a schema model into a URL fragment for the hosted diagram viewer.
//! The CLI builds `{site}/diagram#1.<payload>` where `<payload>` is the model
//! JSON gzip-compressed and base64url-encoded; the site decodes it client-side.
use base64::Engine;
use flate2::{write::GzEncoder, Compression};
use std::io::Write;

use crate::schema_model::SchemaModel;

/// Fragment format version. Bump when the payload encoding changes.
pub const FRAGMENT_VERSION: &str = "1";

/// Encode `model` as base64url(gzip(json)) — the fragment payload (no `1.` prefix).
pub fn encode_payload(model: &SchemaModel) -> Result<String, serde_json::Error> {
    let json = serde_json::to_vec(model)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    // Writing to / finishing an in-memory Vec is infallible.
    enc.write_all(&json).expect("gzip write to Vec");
    let gz = enc.finish().expect("gzip finish to Vec");
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(gz))
}

/// Build the full hosted-viewer URL: `{base}/diagram#1.<payload>`.
pub fn fragment_url(base: &str, model: &SchemaModel) -> Result<String, serde_json::Error> {
    let payload = encode_payload(model)?;
    Ok(format!("{}/diagram#{}.{}", base.trim_end_matches('/'), FRAGMENT_VERSION, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_model::{Column, ProjectInfo, SchemaInfo, SchemaModel, TableNode};
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn sample_model() -> SchemaModel {
        SchemaModel {
            project: ProjectInfo { name: "Acme".to_string(), db: "postgres".to_string(), note: None },
            schemas: vec![SchemaInfo { name: "public".to_string(), tables: 1, enums: 0 }],
            tables: vec![TableNode {
                schema: "public".to_string(),
                name: "users".to_string(),
                kind: "table".to_string(),
                note: None,
                note_md: None,
                columns: vec![Column {
                    name: "id".to_string(), ty: "uuid".to_string(),
                    pk: true, nn: true, en: false, def: None, note: None,
                }],
            }],
            refs: vec![],
        }
    }

    fn decode_payload(payload: &str) -> SchemaModel {
        let gz = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap();
        let mut s = String::new();
        GzDecoder::new(&gz[..]).read_to_string(&mut s).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn encode_payload_round_trips_through_gzip_base64url() {
        let m = sample_model();
        let payload = encode_payload(&m).unwrap();
        let back = decode_payload(&payload);
        assert_eq!(back.project.name, "Acme");
        assert_eq!(back.tables[0].name, "users");
        assert_eq!(back.tables[0].columns[0].pk, true);
    }

    #[test]
    fn fragment_url_has_expected_shape_and_trims_slash() {
        let m = sample_model();
        let url = fragment_url("https://dbd.example/", &m).unwrap();
        assert!(url.starts_with("https://dbd.example/diagram#1."), "got: {url}");
        assert!(!url.contains("//diagram"), "trailing slash not trimmed: {url}");
    }
}
```

- [ ] **Step 3: delete the embedded-HTML assets.**
```bash
git rm crates/dbd-core/assets/diagram.html crates/dbd-core/assets/diagram_viewer.js
```

- [ ] **Step 4: build + test.**
```bash
cargo test -p dbd-core diagram
```
Expected: PASS (2 tests). `cargo build -p dbd-core` succeeds (no more `include_str!` of the deleted files).

- [ ] **Step 5: commit.**
```bash
git add crates/dbd-core/Cargo.toml crates/dbd-core/src/diagram.rs
git commit -m "feat(diagram): encode model to gzip+base64url fragment; drop render_html"
```

---

## Task 2: Rust CLI (`dbd diagram` reshaped)

**Files:**
- Modify: `crates/dbd-cli/Cargo.toml` (add `open`)
- Modify: `crates/dbd-cli/src/cli.rs` (`Diagram` variant)
- Modify: `crates/dbd-cli/src/commands/mod.rs` (dispatch)
- Modify (replace contents): `crates/dbd-cli/src/commands/diagram.rs`

- [ ] **Step 1: dep.** In `crates/dbd-cli/Cargo.toml` `[dependencies]` add:
```toml
open = "5"
```

- [ ] **Step 2: CLI variant.** In `crates/dbd-cli/src/cli.rs`, replace the `Diagram { ... }` variant (currently `file`, `json`) with:
```rust
    Diagram {
        /// Emit the raw SchemaModel JSON to a file instead of opening the viewer
        #[arg(long)]
        json: bool,
        /// Destination file for --json (default: schema.json)
        #[arg(short, long, default_value = "schema.json")]
        file: PathBuf,
        /// Print the viewer URL instead of opening a browser
        #[arg(long)]
        print_url: bool,
        /// Base URL of the dbd site (default: https://dbd.sensei-hq.com)
        #[arg(long, env = "DBD_DIAGRAM_URL")]
        site: Option<String>,
    },
```
(`env = "DBD_DIAGRAM_URL"` makes the flag fall back to the env var; the literal default is applied in the command.)

- [ ] **Step 3: dispatch.** In `crates/dbd-cli/src/commands/mod.rs`, replace the arm:
```rust
        Commands::Diagram { file, json } => {
            diagram::cmd_diagram(config, env, project_dir, file, *json, scope, deps, verbosity)
        }
```
with:
```rust
        Commands::Diagram { json, file, print_url, site } => diagram::cmd_diagram(
            config, env, project_dir, *json, file, *print_url, site.as_deref(), scope, deps, verbosity,
        ),
```

- [ ] **Step 4: replace `commands/diagram.rs`:**
```rust
use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use super::safe_write;
use crate::output::{self, Verbosity};

/// Default hosted dbd site (provisional; override with --site or $DBD_DIAGRAM_URL).
const DEFAULT_SITE: &str = "https://dbd.sensei-hq.com";

/// Resolve the site base URL: an explicit value (flag or $DBD_DIAGRAM_URL, both
/// surfaced by clap as `site`) wins; otherwise the built-in default.
fn resolve_site(site: Option<&str>) -> &str {
    site.unwrap_or(DEFAULT_SITE)
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_diagram(
    config: &Path,
    env: &str,
    project_dir: &Path,
    json: bool,
    file: &Path,
    print_url: bool,
    site: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let model = dbd_core::schema_model::build(&design, Some(&resolved));

    if json {
        let s = serde_json::to_string_pretty(&model).context("Failed to serialize schema model")?;
        safe_write(project_dir, file, &s)?;
        output::info(verbosity, &format!("Wrote schema model to {}", file.display()));
        return Ok(());
    }

    let base = resolve_site(site);
    let url = dbd_core::diagram::fragment_url(base, &model).context("Failed to encode diagram URL")?;
    if url.len() > 1_500_000 {
        output::info(
            verbosity,
            "Note: this schema produces a very large URL; if the browser truncates it, run `dbd diagram --json` and upload the file at the site instead.",
        );
    }
    // The URL is the command's data output — always to stdout (pipeable).
    println!("{url}");
    if !print_url {
        if let Err(e) = open::that(&url) {
            output::info(verbosity, &format!("(couldn't open a browser: {e}); open the URL above)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_site_prefers_explicit_then_default() {
        assert_eq!(resolve_site(Some("http://localhost:5173")), "http://localhost:5173");
        assert_eq!(resolve_site(None), DEFAULT_SITE);
    }
}
```

- [ ] **Step 5: build + test.**
```bash
cargo test -p dbd-cli
cargo build -p dbd-cli
```
Expected: PASS, including the existing `every_subcommand_parses` test (the new `--print-url`/`--site` flags parse). `resolve_site` test passes.

- [ ] **Step 6: manual smoke** (uses the sensei fixture or any project):
```bash
cargo run -p dbd-cli -- diagram --print-url --site http://localhost:5173 -s /Users/Jerry/Developer/sensei-hq/sensei/database
```
Expected: prints `http://localhost:5173/diagram#1.<payload>` and does not open a browser.

- [ ] **Step 7: commit.**
```bash
git add crates/dbd-cli/Cargo.toml crates/dbd-cli/src/cli.rs crates/dbd-cli/src/commands/mod.rs crates/dbd-cli/src/commands/diagram.rs
git commit -m "feat(cli): dbd diagram opens hosted viewer via URL fragment (--print-url/--site/--json)"
```

---

## Task 3: Site fragment codec (`fragment.ts`)

**Files:**
- Create: `site/src/lib/viewer/fragment.ts`
- Create: `site/src/lib/viewer/fragment.test.ts`
- Modify: `site/package.json` (add `fflate`)

- [ ] **Step 1: dep.** In `site/`:
```bash
cd site && bun add fflate
```

- [ ] **Step 2: failing test** `site/src/lib/viewer/fragment.test.ts`:
```ts
import { it, expect } from 'vitest';
import { encodeFragment, decodeFragment } from './fragment';

const model = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 1, enums: 0 }],
  tables: [{ schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] }],
  refs: [],
};

it('round-trips a model through encode/decode', async () => {
  const frag = await encodeFragment(model);
  expect(frag.startsWith('1.')).toBe(true);
  const back = await decodeFragment('#' + frag);
  expect(back).toEqual(model);
});

it('rejects malformed or unknown-version fragments', async () => {
  await expect(decodeFragment('#nope')).rejects.toThrow();
  await expect(decodeFragment('#9.AAAA')).rejects.toThrow();
});
```
Run `bun run test:viewer` → FAIL (module missing).

- [ ] **Step 3: implement `site/src/lib/viewer/fragment.ts`:**
```ts
const VERSION = '1';

function b64urlToBytes(s: string): Uint8Array {
  const pad = '==='.slice((s.length + 3) % 4);
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + pad;
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function bytesToB64url(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  if (typeof DecompressionStream !== 'undefined') {
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'));
    return new Uint8Array(await new Response(stream).arrayBuffer());
  }
  const { gunzipSync } = await import('fflate');
  return gunzipSync(bytes);
}

async function gzip(bytes: Uint8Array): Promise<Uint8Array> {
  if (typeof CompressionStream !== 'undefined') {
    const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream('gzip'));
    return new Uint8Array(await new Response(stream).arrayBuffer());
  }
  const { gzipSync } = await import('fflate');
  return gzipSync(bytes);
}

/** Decode `#1.<base64url-gzip-json>` into a parsed (unvalidated) value. Throws on bad input. */
export async function decodeFragment(hash: string): Promise<unknown> {
  const frag = hash.startsWith('#') ? hash.slice(1) : hash;
  const dot = frag.indexOf('.');
  if (dot < 0) throw new Error('malformed diagram link');
  const version = frag.slice(0, dot);
  if (version !== VERSION) throw new Error(`unsupported diagram link version "${version}"`);
  const gz = b64urlToBytes(frag.slice(dot + 1));
  const json = new TextDecoder().decode(await gunzip(gz));
  return JSON.parse(json);
}

/** Encode a value into a `1.<base64url-gzip-json>` payload (no leading `#`). */
export async function encodeFragment(model: unknown): Promise<string> {
  const json = new TextEncoder().encode(JSON.stringify(model));
  return `${VERSION}.${bytesToB64url(await gzip(json))}`;
}
```
Run `bun run test:viewer` → PASS. (Node ≥ 18 in vitest provides `CompressionStream`/`DecompressionStream`/`Blob`/`Response`; the `fflate` branch is the browser fallback.)

- [ ] **Step 4: commit.**
```bash
git add site/package.json site/bun.lock site/src/lib/viewer/fragment.ts site/src/lib/viewer/fragment.test.ts
git commit -m "feat(viewer): fragment codec (gzip+base64url, DecompressionStream + fflate fallback)"
```

---

## Task 4: Model validation (`validateModel`)

**Files:**
- Modify: `site/src/lib/viewer/model.ts` (add `validateModel`)
- Modify: `site/src/lib/viewer/model.test.ts` (add cases)

- [ ] **Step 1: failing test** — append to `site/src/lib/viewer/model.test.ts`:
```ts
import { validateModel } from './model';

it('accepts a well-formed model and rejects malformed ones', () => {
  const good = { project: { name: 'p', db: 'pg' }, schemas: [], tables: [], refs: [] };
  expect(validateModel(good).ok).toBe(true);
  expect(validateModel(null).ok).toBe(false);
  expect(validateModel({ project: {} }).ok).toBe(false);
  expect(validateModel({ project: { name: 'p' }, schemas: [], tables: [{ name: 'x' }], refs: [] }).ok).toBe(false);
});
```
Run `bun run test:viewer` → FAIL (no `validateModel`).

- [ ] **Step 2: implement** — add to `site/src/lib/viewer/model.ts`:
```ts
export type ValidationResult = { ok: true; model: SchemaModel } | { ok: false; error: string };

/** Shape-check arbitrary JSON before handing it to the viewer. */
export function validateModel(value: unknown): ValidationResult {
  if (typeof value !== 'object' || value === null) return { ok: false, error: 'not a JSON object' };
  const v = value as Record<string, unknown>;
  const project = v.project as Record<string, unknown> | undefined;
  if (!project || typeof project.name !== 'string') return { ok: false, error: 'missing project.name' };
  if (!Array.isArray(v.schemas)) return { ok: false, error: 'missing schemas[]' };
  if (!Array.isArray(v.tables)) return { ok: false, error: 'missing tables[]' };
  if (!Array.isArray(v.refs)) return { ok: false, error: 'missing refs[]' };
  for (const t of v.tables) {
    const tt = t as Record<string, unknown>;
    if (typeof tt.schema !== 'string' || typeof tt.name !== 'string' || !Array.isArray(tt.columns))
      return { ok: false, error: 'malformed table entry' };
  }
  return { ok: true, model: value as SchemaModel };
}
```
Run `bun run test:viewer` → PASS.

- [ ] **Step 3: commit.**
```bash
git add site/src/lib/viewer/model.ts site/src/lib/viewer/model.test.ts
git commit -m "feat(viewer): validateModel shape check"
```

---

## Task 5: Bundled example model (`sample.ts`)

**Files:**
- Create: `site/src/lib/viewer/sample.ts`

- [ ] **Step 1: implement `site/src/lib/viewer/sample.ts`:**
```ts
import type { SchemaModel } from './model';

/** A small example schema so the empty /diagram page can demo the viewer. */
export const SAMPLE_MODEL: SchemaModel = {
  project: { name: 'example', db: 'postgresql' },
  schemas: [{ name: 'shop', tables: 2, enums: 0 }],
  tables: [
    {
      schema: 'shop', name: 'customers', kind: 'table',
      note: 'People who place orders.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true },
        { name: 'email', type: 'text', nn: true },
        { name: 'name', type: 'text' },
      ],
    },
    {
      schema: 'shop', name: 'orders', kind: 'table',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true },
        { name: 'customer_id', type: 'uuid', nn: true },
        { name: 'total', type: 'numeric' },
      ],
    },
  ],
  refs: [{ from: { s: 'shop', t: 'orders', c: 'customer_id' }, to: { s: 'shop', t: 'customers', c: 'id' } }],
};
```

- [ ] **Step 2: type-check** (no runtime test needed; the route test in Task 6 exercises it):
```bash
cd site && bun run check
```
Expected: no new type errors from `sample.ts`.

- [ ] **Step 3: commit.**
```bash
git add site/src/lib/viewer/sample.ts
git commit -m "feat(viewer): bundled example schema model"
```

---

## Task 6: `/diagram` route + full-bleed layout + Viewer de-wiring

**Files:**
- Create: `site/src/routes/diagram/+page.ts`
- Create: `site/src/routes/diagram/+page.svelte`
- Create: `site/src/lib/viewer/diagram.page.test.ts`
- Modify: `site/src/routes/+layout.svelte` (hide Nav/Footer on `/diagram`)
- Modify: `site/src/lib/viewer/Viewer.svelte` (drop standalone theme wiring)

**Use the svelte skill for all `.svelte` edits.**

- [ ] **Step 1: route load opts** — create `site/src/routes/diagram/+page.ts`:
```ts
// Client-only: the model lives in the URL fragment (not available during SSR)
// or is uploaded in-browser. Prerender the shell as a static SPA page.
export const prerender = true;
export const ssr = false;
```

- [ ] **Step 2: de-wire `Viewer.svelte`.** The hosted layout owns theming (`<svelte:body use:themable>` + the `vibe.style` lock), so remove the standalone-only wiring from `site/src/lib/viewer/Viewer.svelte`:
  - Delete the block that sets `vibe.allowedStyles`/`vibe.style` (the `if (typeof window !== 'undefined') { ... }` that locks the style).
  - Delete the `<svelte:body use:themable={{ ... }} />` element.
  - Remove now-unused imports: `themable` (from `@rokkit/actions`) and `vibe` (from `@rokkit/states`) **only if** nothing else in the file references them (the header's `ThemeSwitcherToggle` from `@rokkit/app` manages `vibe` internally and does not import it here). Keep `ThemeSwitcherToggle`.
  - Leave the header, sidebar, diagram, detail, and `dbdLogo` import unchanged.
  - Re-validate with the Svelte MCP autofixer.

- [ ] **Step 3: hide site chrome on `/diagram`** — in `site/src/routes/+layout.svelte`, add the page-path check and gate `Nav`/`Footer`. Add to the `<script>`:
```ts
	import { page } from '$app/state';
	const isApp = $derived(page.url.pathname.startsWith('/diagram'));
```
and wrap the existing `<Nav />` and `<Footer />` in the markup:
```svelte
{#if !isApp}<Nav />{/if}
{@render children()}
{#if !isApp}<Footer />{/if}
```
(Keep the `<svelte:body use:themable={{ theme: vibe, storageKey: 'dbd-theme' }} />` and font/`app.css` imports — they apply to `/diagram` too.)

- [ ] **Step 4: failing test** `site/src/lib/viewer/diagram.page.test.ts`:
```ts
import { it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import { encodeFragment } from './fragment';
import { SAMPLE_MODEL } from './sample';

it('renders the example when "Load example" is clicked', async () => {
  const { getByText, container } = render(Page);
  await fireEvent.click(getByText('Load example'));
  // Sample has two tables → two cards.
  await new Promise((r) => setTimeout(r, 0));
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
});

it('renders a model decoded from the URL fragment', async () => {
  const frag = await encodeFragment(SAMPLE_MODEL);
  window.location.hash = '#' + frag;
  const { container, findAllByText } = render(Page);
  await findAllByText('customers');
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
  window.location.hash = '';
});
```
Run `bun run test:viewer` → FAIL (route missing).

- [ ] **Step 5: implement `site/src/routes/diagram/+page.svelte`:**
```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import Viewer from '$lib/viewer/Viewer.svelte';
  import { decodeFragment } from '$lib/viewer/fragment';
  import { validateModel } from '$lib/viewer/model';
  import { SAMPLE_MODEL } from '$lib/viewer/sample';
  import type { SchemaModel } from '$lib/viewer/model';

  let model = $state<SchemaModel | null>(null);
  let error = $state<string | null>(null);
  let dragging = $state(false);

  function accept(value: unknown) {
    const res = validateModel(value);
    if (res.ok) { model = res.model; error = null; }
    else { error = `Not a valid schema model: ${res.error}`; }
  }

  async function loadFile(file: File) {
    try { accept(JSON.parse(await file.text())); }
    catch { error = 'Could not parse that file as JSON.'; }
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    const file = e.dataTransfer?.files?.[0];
    if (file) loadFile(file);
  }

  function onPick(e: Event) {
    const file = (e.target as HTMLInputElement).files?.[0];
    if (file) loadFile(file);
  }

  onMount(async () => {
    const hash = window.location.hash;
    if (hash.length > 1) {
      try { accept(await decodeFragment(hash)); }
      catch (e) { error = `Could not read the diagram link: ${(e as Error).message}`; }
    }
  });
</script>

<svelte:head><title>dbd — diagram viewer</title></svelte:head>

{#if model}
  <div class="h-screen w-screen">
    <Viewer {model} />
  </div>
{:else}
  <!-- empty / upload state -->
  <main class="grid min-h-screen place-items-center bg-paper p-6 text-ink">
    <div
      role="region"
      aria-label="Upload schema"
      class="w-full max-w-lg rounded-lg border border-dashed p-10 text-center
             {dragging ? 'border-primary bg-accent-soft' : 'border-paper-edge'}"
      ondragover={(e) => { e.preventDefault(); dragging = true; }}
      ondragleave={() => (dragging = false)}
      ondrop={onDrop}
    >
      <h1 class="font-display text-xl font-semibold">Open a schema diagram</h1>
      <p class="mt-2 text-sm text-ink-soft">
        Drop a <code class="font-mono">schema.json</code> here (from
        <code class="font-mono">dbd diagram --json</code>), or
      </p>
      <div class="mt-4 flex items-center justify-center gap-3">
        <label class="cursor-pointer rounded-md bg-primary px-3 py-2 text-sm text-on-primary">
          Choose file
          <input type="file" accept="application/json,.json" class="hidden" onchange={onPick} />
        </label>
        <button
          type="button"
          class="rounded-md border border-paper-edge px-3 py-2 text-sm"
          onclick={() => accept(SAMPLE_MODEL)}
        >Load example</button>
      </div>
      {#if error}<p class="mt-4 text-sm text-danger" data-error>{error}</p>{/if}
    </div>
  </main>
{/if}
```
Run `bun run test:viewer` → PASS. (`[data-card]` is the per-card attribute already emitted by `Diagram.svelte`.)

- [ ] **Step 6: full build sanity.**
```bash
cd site && bun run check && bun run build
```
Expected: type-check clean; `vite build` succeeds and prerenders `/diagram`.

- [ ] **Step 7: commit.**
```bash
git add site/src/routes/diagram/ site/src/routes/+layout.svelte site/src/lib/viewer/Viewer.svelte site/src/lib/viewer/diagram.page.test.ts
git commit -m "feat(site): /diagram route (fragment + upload + example), full-bleed; de-wire Viewer theming"
```

---

## Task 7: Remove the embedded-bundle pipeline

**Files:**
- Delete: `site/vite.viewer.config.ts`
- Modify: `site/package.json` (remove `build:viewer` script + `vite-plugin-css-injected-by-js` dev-dep)
- Modify: `site/src/lib/viewer/viewer.css` (remove base64 `@font-face` block)
- Modify: `site/src/lib/viewer/index.ts` (remove `mountViewer`)
- Modify: `Makefile` (remove `viewer` target)
- Modify: `.github/workflows/ci.yml` (remove `viewer-bundle` job)

- [ ] **Step 1: delete the lib-build config + script + plugin.**
```bash
git rm site/vite.viewer.config.ts
cd site && bun remove vite-plugin-css-injected-by-js
```
Then in `site/package.json` delete the line `"build:viewer": "vite build --config vite.viewer.config.ts",`.

- [ ] **Step 2: drop the base64 fonts** from `site/src/lib/viewer/viewer.css` — delete the entire `@font-face` block (the comment header + the six `@font-face` rules added for the standalone bundle). Keep the `@import '@rokkit/themes/...'` lines at the top and the `[data-mode='dark'] { --on-primary: ... }` override at the bottom. The hosted layout (`+layout.svelte`) already imports the same faces via `@fontsource`, so the route still gets Space Grotesk / IBM Plex Sans / IBM Plex Mono.

- [ ] **Step 3: drop `mountViewer`** from `site/src/lib/viewer/index.ts` (the route imports `Viewer` directly):
```ts
export { default as Viewer } from './Viewer.svelte';
export { type SchemaModel } from './model';
```
(Remove the `mount` import and the `mountViewer` function.)

- [ ] **Step 4: remove the `viewer` Makefile target.** In `Makefile`: delete `viewer` from the `.PHONY` line, delete the `@echo "  make viewer ..."` help line, and delete the `## Rebuild ...` comment + the `viewer:` target body (`@cd site && bun install --frozen-lockfile && bun run build:viewer`).

- [ ] **Step 5: remove the CI job.** In `.github/workflows/ci.yml`, delete the entire `viewer-bundle:` job (name "viewer bundle freshness", the bun setup, `make viewer`, and the `git diff --exit-code crates/dbd-core/assets/diagram_viewer.js` step).

- [ ] **Step 6: verify nothing references the removed pieces.**
```bash
cd /Users/Jerry/Developer/dbd-rs
grep -rn "mountViewer\|build:viewer\|vite.viewer.config\|diagram_viewer.js\|make viewer\|css-injected-by-js" \
  site/src Makefile .github site/package.json crates 2>/dev/null
```
Expected: no matches (other than this plan/spec/docs prose).
```bash
cd site && bun run test:viewer && bun run build
```
Expected: tests PASS; site build succeeds.

- [ ] **Step 7: commit.**
```bash
cd /Users/Jerry/Developer/dbd-rs
git add site/package.json site/bun.lock site/src/lib/viewer/viewer.css site/src/lib/viewer/index.ts Makefile .github/workflows/ci.yml
git rm --cached --ignore-unmatch site/vite.viewer.config.ts
git commit -m "chore(viewer): remove embedded lib-bundle pipeline (build:viewer, make viewer, CI freshness, base64 fonts)"
```

---

## Task 8: Docs

**Files:**
- Modify: `docs/guide/04-commands.md` (the `dbd diagram` section)
- Modify: `docs/llms/llms.txt`, `docs/llms/llms-full.txt`
- Modify: `README.md` (the `dbd diagram` table row)

- [ ] **Step 1: rewrite the `dbd diagram` section** in `docs/guide/04-commands.md` to:
```md
## `dbd diagram`

Open the schema in the **hosted interactive viewer** — sidebar schema→table navigation, a pannable/zoomable ER diagram, and a per-table detail panel. The model is gzip-compressed into the URL fragment (client-side only, never sent to a server), so the link is private and self-contained.

```sh
dbd diagram                        # build the model and open it in your browser
dbd diagram --print-url            # print the viewer URL instead of opening a browser
dbd diagram --site http://localhost:5173   # point at a different site (or set $DBD_DIAGRAM_URL)
dbd diagram --json -f schema.json  # write the raw SchemaModel JSON (upload it at <site>/diagram)
dbd diagram --scope hub            # scope-aware (only the scope's tables/refs)
```

`dbd diagram` prints the URL and opens your default browser; on a headless machine use `--print-url`. The `--json` output is the dbd-native schema model (schemas, tables, columns, FK refs); upload it on the site's `/diagram` page, or feed it to other tooling. The model is JSON (not DBML), so it extends to views/functions/procedures later.
```

- [ ] **Step 2: update `README.md`** — change the `dbd diagram` table row to:
```md
| `dbd diagram` | Open the schema in the hosted interactive viewer (`--print-url` to print the link, `--json` for the raw model) |
```

- [ ] **Step 3: update `docs/llms/llms.txt`** — replace the `dbd diagram` bullet with:
```
- `dbd diagram` — build the schema model and open the hosted interactive viewer (model gzip-encoded in the URL fragment, client-side only); `--print-url` prints the link, `--site`/`$DBD_DIAGRAM_URL` overrides the base, `--json -f` writes the raw SchemaModel JSON for upload/tooling
```
and make the equivalent edit to the `dbd diagram` description in `docs/llms/llms-full.txt`.

- [ ] **Step 4: verify the doc copy step + build.**
```bash
cd site && node scripts/copy-content.mjs && bun run build
```
Expected: content sync + build succeed (the guide/llms changes flow into the site).

- [ ] **Step 5: commit.**
```bash
cd /Users/Jerry/Developer/dbd-rs
git add docs/guide/04-commands.md docs/llms/llms.txt docs/llms/llms-full.txt README.md
git commit -m "docs: dbd diagram opens the hosted viewer (URL fragment); --json for the raw model"
```

---

## Self-review checklist (run before handoff)

- **Spec coverage:** encode contract ✓ (T1); CLI surface — default-open/`--print-url`/`--json`/`--site`/`$DBD_DIAGRAM_URL`/headless fallback/oversized warning ✓ (T2); `/diagram` route — fragment + upload + example + validation ✓ (T3–T6); removals — local HTML/assets ✓ (T1), lib bundle/`build:viewer`/`make viewer`/CI/base64 fonts/`mountViewer` ✓ (T7); docs ✓ (T8); tests — Rust round-trip + `resolve_site` + fragment round-trip + `validateModel` + route render ✓.
- **Integration gaps resolved:** the Viewer's standalone theme wiring is removed so it doesn't double-wire against the layout's `themable` (T6/Step 2); `/diagram` renders full-bleed by hiding Nav/Footer in the root layout (T6/Step 3), so the Viewer header is the only chrome (no duplicate theme toggle).
- **Type/name consistency:** `encode_payload`/`fragment_url`/`FRAGMENT_VERSION` (Rust) and `encodeFragment`/`decodeFragment` + the `"1."` version prefix (TS) agree; `resolve_site`/`DEFAULT_SITE`/`DBD_DIAGRAM_URL` consistent across cli.rs and diagram.rs; `validateModel`/`ValidationResult`, `SAMPLE_MODEL`, `[data-card]` used consistently in the route + test.
- **No placeholders:** every code step carries full code; commands have expected output.

## Done when

`dbd diagram` prints `https://dbd.sensei-hq.com/diagram#1.<payload>` and opens the browser; `--print-url` prints without opening; `--json -f` still writes the model file; `/diagram` renders the viewer from a fragment, an uploaded `schema.json`, or the bundled example, with friendly errors on bad input; the embedded-HTML path, lib-bundle build, `make viewer`, CI freshness job, and base64 fonts are gone; `cargo test --workspace` + clippy green; `bun run test:viewer` + `bun run build` green. Then release (`make bump`).
