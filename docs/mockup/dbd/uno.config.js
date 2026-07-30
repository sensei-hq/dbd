/**
 * dbd — single source of truth for design tokens.
 *
 * Used two ways:
 *   Browser (design preview): this file assigns window.__unocss, then
 *     @unocss/runtime picks it up. Load THIS before the runtime script.
 *   Build (app repo): `const config = require('./uno.config.js')` — or paste the
 *     object into uno.config.ts and wrap with defineConfig(). Same tokens,
 *     same shortcuts, same preflight.
 *
 * Rules of the road:
 *   - No raw colors in markup. Use semantic names: bg / surface / line / fg /
 *     muted / faint / accent / action / on-action / tok-*.
 *   - No raw font sizes. Use the named scale: eyebrow / caption / code / cmd /
 *     body / base / lede / title-sm / title / title-lg / head / display.
 *   - No media queries. Use lt-lg: (<960) lt-md: (<820) lt-sm: (<680).
 *   - Repeated compositions live in `shortcuts`, not in markup.
 *
 * TYPOGRAPHY — self-hosted via Fontsource. Install exactly these:
 *   npm i @fontsource/space-grotesk @fontsource/ibm-plex-sans @fontsource/ibm-plex-mono
 * and import exactly these weights (see `fonts` export at the bottom):
 *   space-grotesk  400 500 600 700   -> font-display
 *   ibm-plex-sans  400 500 600       -> font-sans (body default)
 *   ibm-plex-mono  400 500 600       -> font-mono
 * Nothing in the design uses a weight outside that list. The preview loads the
 * same Fontsource files from jsDelivr, so preview and app render identically.
 */
(function () {
  var preflight = `
:root { --accent-h: 245; --accent-c: 0.12; --section-y: 7rem; }

:root, [data-theme="light"] {
  --bg:oklch(0.987 0.006 85); --bg-deep:oklch(0.967 0.008 85);
  --surface:oklch(0.996 0.004 85); --surface-2:oklch(0.978 0.006 85);
  --line:oklch(0.905 0.009 85); --line-soft:oklch(0.942 0.007 85);
  --fg:oklch(0.25 0.012 80); --muted:oklch(0.48 0.014 80); --faint:oklch(0.62 0.014 80);
  --band:oklch(0.955 0.008 85 / 0.55);
  --accent:oklch(0.55 0.13 var(--accent-h));
  --accent-2:oklch(0.49 0.135 var(--accent-h));
  --on-accent:oklch(0.99 0.012 var(--accent-h));
  --accent-soft:oklch(0.93 0.035 var(--accent-h));
  --accent-line:oklch(0.83 0.06 var(--accent-h));
  --action:oklch(0.55 0.13 var(--accent-h));
  --action-hover:oklch(0.49 0.135 var(--accent-h));
  --on-action:oklch(0.99 0.012 var(--accent-h));
  --code-bg:oklch(0.97 0.008 85);
  --tok-comment:oklch(0.6 0.012 80);
  --tok-key:oklch(0.5 0.13 var(--accent-h));
  --tok-str:oklch(0.50 0.11 155); --tok-num:oklch(0.50 0.12 250);
  --tok-punct:oklch(0.5 0.012 80);
  --tok-arrow:oklch(0.5 0.13 var(--accent-h));
}

[data-theme="dark"] {
  --bg:oklch(0.185 0.035 245); --bg-deep:oklch(0.15 0.035 245);
  --surface:oklch(0.225 0.037 245); --surface-2:oklch(0.27 0.038 245);
  --line:oklch(0.325 0.04 245); --line-soft:oklch(0.27 0.035 245);
  --fg:oklch(0.965 0.006 240); --muted:oklch(0.74 0.014 245); --faint:oklch(0.58 0.016 245);
  --band:oklch(0.15 0.035 245 / 0.5);
  --accent:oklch(0.78 var(--accent-c) var(--accent-h));
  --accent-2:oklch(0.70 var(--accent-c) var(--accent-h));
  --on-accent:oklch(0.17 0.02 var(--accent-h));
  --accent-soft:oklch(0.30 0.05 var(--accent-h));
  --accent-line:oklch(0.42 0.07 var(--accent-h));
  /* accent buttons: solid mid-accent fill, near-white label in BOTH themes */
  --action:oklch(0.52 var(--accent-c) var(--accent-h));
  --action-hover:oklch(0.46 var(--accent-c) var(--accent-h));
  --on-action:oklch(0.99 0.012 var(--accent-h));
  --code-bg:oklch(0.16 0.035 245);
  --tok-comment:oklch(0.56 0.016 245);
  --tok-key:oklch(0.78 var(--accent-c) var(--accent-h));
  --tok-str:oklch(0.80 0.09 155); --tok-num:oklch(0.80 0.09 220);
  --tok-punct:oklch(0.62 0.014 245);
  --tok-arrow:oklch(0.70 var(--accent-c) var(--accent-h));
}

[data-accent="sky"]    { --accent-h:245; --accent-c:0.12; }
[data-accent="rust"]   { --accent-h:52;  --accent-c:0.135; }
[data-accent="green"]  { --accent-h:155; --accent-c:0.115; }
[data-accent="violet"] { --accent-h:300; --accent-c:0.115; }

[data-density="compact"]     { --section-y:4.5rem; }
[data-density="comfortable"] { --section-y:7rem; }
[data-density="spacious"]    { --section-y:9.5rem; }

/* A custom "preflights" array REPLACES the preset reset, so the parts the
   utilities depend on have to live here. Do not delete this rule: without
   box-sizing, w-full + px-* overflows its container; without border-style,
   every border utility collapses to nothing. */
*, ::before, ::after { box-sizing: border-box; border-style: solid; border-width: 0; }

/* Component mounts must not form a layout box of their own: a section's sticky
   positioning and full-bleed background belong to the page flow, not to a
   71px-tall wrapper. (Runtime integration only — a React build has no wrapper.) */
.dbd-root > .sc-host, .dbd-root main > .sc-host { display: contents; }

html { scroll-behavior: smooth; -webkit-text-size-adjust: 100%; }
body { margin: 0; font-family: 'IBM Plex Sans', system-ui, sans-serif; -webkit-font-smoothing: antialiased; color: var(--fg); }
/* :where() keeps these at zero specificity so color utilities still win on links */
.dbd-root :where(a) { color: inherit; text-decoration: none; }
.dbd-root :where(a:hover) { color: var(--accent); }
.dbd-root ::selection { background: var(--accent-soft); color: var(--fg); }
.dbd-root pre { font-variant-ligatures: none; }

@keyframes dbdRise { from { opacity:0; transform:translateY(14px); } to { opacity:1; transform:none; } }
@media (prefers-reduced-motion: reduce) { .animate-rise { animation: none !important; } }
`;

  var config = {
    theme: {
      breakpoints: { sm: '680px', md: '820px', lg: '960px' },

      colors: {
        bg: 'var(--bg)',
        'bg-deep': 'var(--bg-deep)',
        // translucent header fill, sits over blurred content
        header: 'color-mix(in oklch, var(--bg) 82%, transparent)',
        surface: 'var(--surface)',
        'surface-2': 'var(--surface-2)',
        band: 'var(--band)',
        line: 'var(--line)',
        'line-soft': 'var(--line-soft)',
        'accent-line': 'var(--accent-line)',
        fg: 'var(--fg)',
        muted: 'var(--muted)',
        faint: 'var(--faint)',
        accent: 'var(--accent)',
        'accent-2': 'var(--accent-2)',
        'accent-soft': 'var(--accent-soft)',
        'on-accent': 'var(--on-accent)',
        action: 'var(--action)',
        'action-hover': 'var(--action-hover)',
        'on-action': 'var(--on-action)',
        'code-bg': 'var(--code-bg)',
        tok: {
          comment: 'var(--tok-comment)',
          key: 'var(--tok-key)',
          str: 'var(--tok-str)',
          num: 'var(--tok-num)',
          punct: 'var(--tok-punct)',
          arrow: 'var(--tok-arrow)',
        },
      },

      fontFamily: {
        display: "'Space Grotesk', system-ui, sans-serif",
        sans: "'IBM Plex Sans', system-ui, sans-serif",
        mono: "'IBM Plex Mono', ui-monospace, monospace",
      },

      // only these weights are shipped by Fontsource — don't use others
      fontWeight: { normal: '400', medium: '500', semibold: '600', bold: '700' },

      fontSize: {
        eyebrow: ['0.72rem', '1'],
        caption: ['0.8rem', '1.5'],
        code: ['0.82rem', '1.6'],
        cmd: ['0.85rem', '1.5'],
        body: ['0.9rem', '1.6'],
        base: ['1rem', '1.65'],
        lede: ['1.15rem', '1.6'],
        'title-sm': ['1.15rem', '1.4'],
        title: ['1.4rem', '1.25'],
        'title-lg': ['1.55rem', '1.25'],
        head: ['clamp(1.9rem, 3.2vw, 2.7rem)', '1.08'],
        display: ['clamp(2.6rem, 6vw, 4.6rem)', '0.98'],
      },

      letterSpacing: {
        display: '-0.035em',
        head: '-0.022em',
        title: '-0.015em',
        brand: '-0.01em',
        eyebrow: '0.14em',
      },

      borderRadius: { card: '1.1rem' },

      maxWidth: {
        shell: '76rem',
        prose: '42rem',
        copy: '36rem',
        note: '20rem',
      },

      // py-section follows the density tweak
      spacing: { section: 'var(--section-y)' },
    },

    rules: [
      ['text-balance', { 'text-wrap': 'balance' }],
      ['text-pretty', { 'text-wrap': 'pretty' }],
      ['animate-rise', { animation: 'dbdRise .7s cubic-bezier(.2,.7,.2,1) both' }],
      ['bg-grid', {
        'background-image':
          'linear-gradient(to right, var(--line-soft) 1px, transparent 1px),' +
          'linear-gradient(to bottom, var(--line-soft) 1px, transparent 1px)',
        'background-size': '64px 64px',
      }],
    ],

    shortcuts: {
      // layout
      shell: 'mx-auto w-full max-w-shell px-6',
      'section-band': 'relative z-1 bg-band',
      'section-plain': 'relative z-1',

      // type
      eyebrow: 'flex items-center gap-2.5 font-mono text-eyebrow font-medium uppercase tracking-eyebrow text-accent',
      dot: 'inline-block size-1.5 flex-none rounded-full bg-accent',
      h1: 'font-display font-bold text-display tracking-display text-fg text-balance m-0',
      h2: 'font-display font-semibold text-head tracking-head text-fg text-balance m-0',
      h3: 'font-display font-semibold text-title text-fg m-0',
      h4: 'font-display font-medium text-title-sm text-fg m-0',
      lede: 'text-lede text-muted text-pretty m-0',
      copy: 'text-body text-muted text-pretty m-0',
      kicker: 'font-mono text-eyebrow uppercase tracking-eyebrow text-faint',

      // surfaces
      card: 'flex flex-col rounded-card border border-line bg-surface p-6',
      frame: 'overflow-hidden rounded-card border border-line bg-code-bg',
      chip: 'w-fit rounded-md bg-code-bg px-2.5 py-1 font-mono text-caption text-accent',
      badge: 'grid size-7 flex-none place-items-center rounded-md bg-accent-soft font-mono text-accent',

      // buttons
      btn: 'inline-flex flex-none items-center justify-center gap-2 rounded-lg font-medium whitespace-nowrap',
      'btn-accent': 'btn bg-action text-on-action hover:bg-action-hover hover:text-on-action',
      'btn-ghost': 'btn border border-line bg-transparent text-fg',

      // section head cluster
      'head-start': 'flex flex-col items-start gap-4',
      'head-center': 'flex flex-col items-center gap-4 text-center',

      // card grids
      'grid-3': 'grid gap-4 grid-cols-3 lt-lg:grid-cols-2 lt-sm:grid-cols-1',
      'grid-4': 'grid gap-4 grid-cols-4 lt-lg:grid-cols-2 lt-sm:grid-cols-1',
      'grid-2': 'grid gap-4 grid-cols-2 lt-sm:grid-cols-1',
    },

    preflights: [{ getCSS: function () { return preflight; } }],
  };

  if (typeof window !== 'undefined') window.__unocss = config;
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = config;
    // handoff manifest: the exact Fontsource packages + weights this design uses
    module.exports.fonts = [
      { pkg: '@fontsource/space-grotesk', family: 'Space Grotesk', token: 'font-display', weights: [400, 500, 600, 700] },
      { pkg: '@fontsource/ibm-plex-sans', family: 'IBM Plex Sans', token: 'font-sans', weights: [400, 500, 600] },
      { pkg: '@fontsource/ibm-plex-mono', family: 'IBM Plex Mono', token: 'font-mono', weights: [400, 500, 600] },
    ];
  }
})();
