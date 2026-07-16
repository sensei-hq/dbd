# Zen-Sumi Design System

A design system for **Sensei** — a local-first desktop companion that observes your AI-assisted coding sessions and quietly surfaces patterns. The system's internal name is **Zen-Sumi** ("zen" + 墨 *sumi*, ink).

The aesthetic is washi paper, sumi ink, and a single rationed vermillion accent (朱 *shu*). It borrows from Japanese restraint: hairlines over shadows, generous negative space (間 *ma*), kanji as small functional marks, and "less, but better" copy.

> The master observes for a long time before teaching.

The compiler namespace for consuming projects is **`ZenSumiDesignSystemSensei_9d9b38`** — mount components with `const { Button } = window.ZenSumiDesignSystemSensei_9d9b38`.

## Source materials

This system was distilled from a working prototype in the **Sensei / Zen-Sumi** project (`00576ec1-3d4c-42ba-bc50-c1b980277c42`). That project's `sensei_src/` contains the original design canvases:

- **`Sensei Observatory.html`** — the desktop app design canvas (~30 artboards: bootstrap, wizard, observatory, project window, sessions, learnings, instruments, libraries).
- **`Sensei Site.html`** — three marketing-site variants.
- `colors_and_type.css` — the prototype's monolithic stylesheet.
- 50+ JSX files implementing screens and data.

The prototype had grown **30+ font sizes** and a chaotic spacing set, and its stylesheet had drifted (tokens renamed to `--paper-soft` / `--ink-line` while the JSX still referenced the older numbered names, so some references resolved to nothing). **This system consolidates both:** a tight, semantic token set on the numbered scheme the components actually use, split into `tokens/` modules, with the eight-size type scale and nine-stop spacing scale as the whole budget.

## Products

| Product | What it is | UI kit |
|---|---|---|
| **Sensei Observatory** | Tauri desktop app · the daily "morning briefing", sessions, learnings, instruments, libraries | `ui_kits/observatory/` |
| **Sensei Site** | Marketing website · hero, what-it-is, Watch · Notice · Adopt, privacy, pricing, FAQ | `ui_kits/site/` |

---

## Components

The reusable React primitives (mount via `window.ZenSumiDesignSystemSensei_9d9b38`). Each has a `.d.ts` contract and a `.prompt.md` usage note beside it.

- **core/** — `Button`, `IconButton`, `Badge`, `Kanji`, `StatusDot`, `Eyebrow`
- **forms/** — `Input`
- **surfaces/** — `Card`
- **data/** — `Insight` (the signature meaningful-badge row), `Sparkline`
- **app/** — `WindowChrome` (the 38px Tauri title bar)

`Button`, `Card`, and `Insight` are also registered as **starting points**. The plain-CSS equivalents of every component live in `components.css` (`.zs-btn`, `.zs-card`, `.zs-input`, `.zs-badge`, `.zs-dot`, `.zs-chrome`) — treat that file as the shared contract when working in a non-React context.

---

## VISUAL FOUNDATIONS

### The vibe

Washi paper, sumi ink, vermillion brush mark. Quiet, calm, generous with whitespace. The interface feels like a notebook you'd keep on your desk, not a SaaS dashboard. No data-slop, no animated gradients, no rounded-corner emoji cards. The product *is* restraint.

### Color

A **paper / ink / accent** semantic system over a raw `washi`, `sumi`, `shu` palette in OKLCH (`tokens/colors.css`).

- `--paper` is the page (warm off-white). `--paper-2` raises a step (cards, inputs); `--paper-3` sinks a step (hover, inset). `--edge` is the hairline color.
- `--ink` → `--ink-2` → `--ink-3` → `--ink-4` is the four-step ink ladder (primary → body → meta → faint). Hue is slightly warm, chroma very low. **Never pure black; never pure grey.**
- `--accent` is 朱 vermillion — the *only* hue in the system. Rationed: kanji marks, the active dot, primary CTAs, a focus ring. Never a background fill at scale, rarely twice in one view.
- `--success` (jade) and `--warning` (amber) exist for status but appear sparingly. There is no separate "info" or "danger" stack.

Light and dark share the same semantic names; the values flip (`[data-theme="dark"]`). Light is canonical.

### Type

- **Display:** *Fraunces* (variable serif), weights 300–500, used above `--text-lg`, always tight tracking (`-0.02em`).
- **UI:** *Inter*, 400–600 — default body and chrome.
- **Mono:** *JetBrains Mono*, tabular figures — numbers, IDs, paths, keyboard hints, versions. Never paragraphs.
- **Kanji:** *Shippori Mincho* (→ Yu Mincho → Hiragino Mincho ProN → Songti SC → serif) for the small functional kanji marks.

The **type scale is 8 sizes**: `xs 11 / sm 13 / base 15 / lg 17 / xl 22 / 2xl 28 / 3xl 40 / 4xl 56`. Below `xs` and between sizes are disallowed. Kanji and Latin never share a glyph run — kanji are always wrapped in `<span class="zs-kanji">` (or the `<Kanji/>` component).

### Spacing

A **9-stop scale** on a 4px base: `4 / 8 / 12 / 16 / 24 / 32 / 48 / 64 / 96`. That is the entire budget — no `18px`, no `26px`, no `5px`. Component padding lands at `--space-3` (small UI), `--space-4` (cards), `--space-5` (section headers), `--space-7` (between sections).

### Backgrounds

Almost always solid paper (`--paper` or `--paper-2`). The system does **not** use full-bleed photography, gradients, repeating patterns, or generative texture. A section that needs more presence gets a `--paper-2` lift and a hairline — not a colored background. The one rare exception is a very large kanji glyph at high opacity, used as a watermark.

### Animation

Quiet. One easing curve `--ease` (`cubic-bezier(0.2,0.6,0.2,1)`), three durations (`--dur-fast 120 / --dur 180 / --dur-slow 280`). **Hover** = background lift to `--paper-3` or one-step-darker ink. **Press** = `translateY(0.5px)`, never scale, never glow. **Enter** = opacity + 4px `translateY`. No spring, no bounce, no stagger, no skeleton loaders (faint copy like "Still listening." stands in).

### Borders, hairlines, shadows

The system **prefers hairlines to shadows.** `--hairline` (`1px solid var(--edge)`) separates rows, divides sections, outlines cards. Shadows are reserved **only** for things that physically lift off the page — modals, command palettes, focus overlays (`--shadow`, `--shadow-lg`). **Cards do not shadow. Buttons do not shadow.**

### Radii

`--radius-sm 4 / --radius 6 / --radius-lg 10 / --radius-full`. Most UI uses `--radius`. Cards use `--radius-lg`. Pills and avatars use `--radius-full`. Square corners are valid for hairline-only sections.

### Transparency & blur

Sparing. Modal overlay: `oklch(0.22 0.012 50 / 0.28)` + 1px blur beneath. Soft badge tints: `--accent-soft`, `--success-soft`, `--warning-soft`. No `backdrop-filter` on chrome by default — the system is happier on opaque paper.

### Layout

- Marketing site: `--site-width` 1100px content column, 48px gutters.
- Desktop app: full-bleed, a `--sidebar-width` 240px rail + main view, separated by hairlines.
- The Tauri window chrome (`.zs-chrome` / `<WindowChrome/>`) is always `--chrome-height` 38px, traffic-lights left, centered subtitle.
- Desktop-first; the prototype targets 1280–1600 wide.

### Cards

A card is `background: var(--paper-2)` + `border: var(--hairline)` + `border-radius: var(--radius-lg)` + `padding: var(--space-5)`. That is the whole specification. **No shadow. No colored left-borders. No header background fill.**

### Lists, rows, tables

Rows are separated by hairlines, not zebra striping. Selected and hovered rows lift to `--paper-3`. The hairline ladder is the primary list pattern.

---

## CONTENT FUNDAMENTALS

### Voice

**Patient, quiet, observant.** Sensei sounds like a wise teacher who speaks rarely. It almost never uses the word "AI", never says "powered by", never uses exclamation marks. When it has nothing to say, it stays silent — the empty state is content. *"Still listening."* is a feature.

### Person & casing

- **Second person.** "Your sessions stay on your machine." "You decide what's signal."
- Sensei refers to itself in **lowercase third person** — "sensei watches", "sensei has learned" — never "I". The product is a presence, not a personality.
- **Sentence case** everywhere except the proprietary mark "Sensei" and kanji-titled phases (Watch · Notice · Adopt).
- Eyebrow labels are UPPERCASE with `0.18em` tracking — but only the eyebrow, not the heading beneath.
- Section headings are sentence-case fragments: "One desktop app. One quiet promise." — not "About Sensei".

### Tone examples

| Surface | Copy |
|---|---|
| Hero | "A quiet companion for AI-assisted work." |
| Hero sub | "Sensei watches your sessions with AI assistants — then surfaces the patterns you're too close to see. Not a chatbot. Not a copilot. A patient observer." |
| CTA | "Download for macOS" |
| Pricing | "Free. Pay what feels right." |
| Privacy | "Your sessions stay on your machine." |
| In-app koan | "The AI does not know your auth." |
| Empty state | "Still listening." |
| Footer | `先生 Sensei v0.4.2` |

Structure: a kanji anchor, a one-line fragment that lands, a calm second sentence that explains. Periods, not exclamations.

### What sensei does NOT say

- "Unlock", "supercharge", "10x", "revolutionize"
- "AI-powered", "AI-native", "AI-first"
- "Welcome!", "Hi there!", "Let's get started!"
- "Loading…" (uses "Still listening." instead)
- Emoji, ever. The kanji do the affective work.

### Characters

No emoji, anywhere. Numbers one–nine spell out in body copy; digits for stats and IDs. Em dashes ( — ) for parentheticals, en dashes (–) for ranges, never ASCII `--`. Curly quotes. The center-dot ( · ) is a liberal separator: `lumen-auth · 38m · 3 corrections`. Arrows are unicode (`↓ ↑ → ←`).

### Microcopy

Numbers are *meaningful*. Sensei deliberately replaces big-number stats with small, specific phrases:

> "Cache invalidation missed again in session s-2891. **3rd time.**"

That `3rd time` badge tells you everything — don't dilute it with another count. This is exactly what the `Insight` component's `tag` prop encodes.

---

## ICONOGRAPHY

Sensei has **three layers** of iconographic vocabulary, in priority order.

### 1 · Kanji (the brand layer)

Single Mincho-serif characters, rendered via `--font-kanji` (the `<Kanji/>` component or `.zs-kanji`). Each has a fixed meaning — full set in `assets/kanji.md`. Highlights: 先生 sensei (brand), 観 observe, 察 notice, 覚 adopt, 場 projects, 刻 sessions, 具 instruments, 蔵 archive, 静 stillness, 空 empty. Kanji are `--accent` when functional, `--ink-3` when decorative, `--ink-4` when disabled. Never render below ~14px — the brush detail dies. Never invent a new kanji; use a sentence instead.

### 2 · Solar Icons Bold Duotone (the working icon set)

For everyday affordances (close, search, add, settings, download, archive, check, warning) the system uses **Solar Icons Bold Duotone** from the Iconify CDN — two-tone, rounded silhouettes that read softer on washi than a line set:

```html
<img src="https://api.iconify.design/solar:eye-bold-duotone.svg?color=%232A2925" width="24" height="24" alt=""/>
```

Tint via `?color=` to match the ink/accent scale (`--ink` ≈ `#2A2925`, `--accent` ≈ `#A83D1F`). **Use the `-bold-duotone` variant only** — Linear/Outline/Bold are too sharp and break tone.

### 3 · Custom brush marks (three locked zen-garden anchors)

For the three central phases, hand-built brush SVGs in `assets/glyphs.svg`, referenced via `<use href="assets/glyphs.svg#zen-…"/>`:

| Concept | Glyph | Symbol |
|---|---|---|
| **observe** | `#zen-bonsai` | a thing cultivated by sustained attention |
| **notice** | `#zen-rake` | a pattern emerging from raked sand |
| **memory** | `#zen-pagoda` | stored teachings, stacked in tiers |

Both stroke and fill use `currentColor`, so tinting is a single CSS rule. These three render `--accent` when active, `--ink` when documentary, `--ink-3` when disabled.

### Forbidden

Emoji · Material / Font Awesome / Phosphor / Heroicons / Lucide / Feather · icons on rounded-corner colored squares · gradient-filled icons · 3D/isometric illustration · stock photography · mixing Solar variants.

---

## Substitutions / open questions

- **Fonts** are loaded from **Google Fonts** at runtime (`tokens/fonts.css`) — no self-hosted `.woff2` shipped in the source. *Shippori Mincho* stands in for the kanji marks (an upgrade over the raw system Mincho stack). **If production prefers self-hosted fonts, please supply the `.woff2` files and we'll add `@font-face` rules.** Because fonts come in via `@import`, the compiler reports **0 local `@font-face` fonts** — this is expected.
- **Kanji font:** *Shippori Mincho* first, then the system Mincho stack. On Windows without either, the generic serif fallback is acceptable but less elegant.

---

## Index

- `styles.css` — the entry point consumers link (a list of `@import`s only).
- `tokens/` — `fonts · colors · typography · spacing · radius · elevation · motion`.
- `base.css` · `utilities.css` · `components.css` — reset + type classes, utility layer, component classes.
- `components/` — the React primitives (`core · forms · surfaces · data · app`), each with `.jsx` + `.d.ts` + `.prompt.md` + a `@dsCard` HTML.
- `foundations/` — the specimen cards for the Design System tab (Colors · Type · Spacing · Brand).
- `ui_kits/observatory/` · `ui_kits/site/` — the two product recreations.
- `assets/` — `brandmark.svg`, `glyphs.svg`, `kanji.md`.
- `SKILL.md` — the agent-skill manifest (also usable in Claude Code).

### How to use

1. Link `styles.css`.
2. Wrap your root in `<div class="zs">` so tokens and utilities resolve.
3. Prefer the semantic classes (`text-ink-2`, `bg-paper-2`, `zs-card`, `zs-btn-primary`) and the React components over raw values.
4. If you need a value that isn't in the scale — don't add it. Pick the nearest stop, or simplify. **The constraint is the system.**
