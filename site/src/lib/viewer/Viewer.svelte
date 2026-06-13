<script lang="ts">
  import { vibe } from '@rokkit/states';
  import { themable } from '@rokkit/actions';
  import { ThemeSwitcherToggle } from '@rokkit/app';
  import Sidebar from './Sidebar.svelte';
  import Diagram from './Diagram.svelte';
  import Detail from './Detail.svelte';
  import { createViewerState } from './state.svelte';
  import type { SchemaModel } from './model';
  import type { Density, Arrange } from './layout';
  import './styles.css';
  // Inline the logo so the standalone HTML bundle stays self-contained (no
  // external URL); Vite resolves `?raw` to the file's text content.
  import dbdLogo from '../../../static/dbd.svg?raw';

  let { model }: { model: SchemaModel } = $props();
  const state = createViewerState();

  // The site ships only the zen-sumi style; lock vibe to it so the named tokens
  // resolve in the standalone HTML (mirrors site/src/routes/+layout.svelte).
  if (typeof window !== 'undefined') {
    vibe.allowedStyles = ['zen-sumi'];
    vibe.style = 'zen-sumi';
  }

  const tableCount = $derived(model.tables.length);
  const enumCount = $derived(model.schemas.reduce((a, s) => a + s.enums, 0));
  const refCount = $derived(model.refs.length);

  const DENSITIES = ['names', 'keys', 'full'] as const satisfies readonly Density[];
  const ARRANGES = ['untangle', 'a-z'] as const satisfies readonly Arrange[];

  function closeDetail() {
    state.selected = null;
    state.mode = 'overview';
  }
</script>

<svelte:body use:themable={{ theme: vibe, storageKey: 'dbd-diagram-theme' }} />

<div class="vw-app flex h-screen flex-col overflow-hidden bg-paper text-ink">
  <!-- header -->
  <header
    data-viewer-header
    class="flex flex-none items-center gap-4 border-b border-paper-edge bg-paper-soft px-4 py-2"
  >
    <!-- eslint-disable-next-line svelte/no-at-html-tags — inlined trusted local SVG asset -->
    <span class="vw-logo flex-none" aria-hidden="true">{@html dbdLogo}</span>
    <span class="font-display font-semibold">{model.project.name}</span>
    <span class="rounded-full bg-accent-soft px-2 text-xs text-primary">{model.project.db}</span>
    <span class="font-mono text-xs text-ink-soft">
      {tableCount} tables · {enumCount} enums · {refCount} refs
    </span>

    <div class="ml-auto flex items-center gap-4">
      <!-- density -->
      <div class="flex items-center gap-2">
        <span class="font-mono text-[0.62rem] uppercase tracking-wide text-ink-soft">density</span>
        <div class="vw-seg flex overflow-hidden rounded-md border border-paper-edge">
          {#each DENSITIES as d (d)}
            <button
              type="button"
              data-density={d}
              aria-pressed={state.density === d}
              class="px-2 py-1 font-mono text-xs {state.density === d
                ? 'bg-primary text-on-primary'
                : 'bg-paper-mute text-ink-mute'}"
              onclick={() => (state.density = d)}
            >
              {d}
            </button>
          {/each}
        </div>
      </div>

      <!-- arrange -->
      <div class="flex items-center gap-2">
        <span class="font-mono text-[0.62rem] uppercase tracking-wide text-ink-soft">arrange</span>
        <div class="vw-seg flex overflow-hidden rounded-md border border-paper-edge">
          {#each ARRANGES as a (a)}
            <button
              type="button"
              data-arrange={a}
              aria-pressed={state.arrange === a}
              class="px-2 py-1 font-mono text-xs {state.arrange === a
                ? 'bg-primary text-on-primary'
                : 'bg-paper-mute text-ink-mute'}"
              onclick={() => (state.arrange = a)}
            >
              {a}
            </button>
          {/each}
        </div>
      </div>

      <ThemeSwitcherToggle variant="triad" />
    </div>
  </header>

  <!-- body -->
  <div class="relative flex min-h-0 flex-1">
    <aside class="vw-aside w-64 flex-none overflow-hidden">
      <Sidebar {model} {state} />
    </aside>

    <main class="relative min-h-0 min-w-0 flex-1">
      <Diagram {model} {state} />

      <!-- detail slide-over -->
      {#if state.selected}
        <aside
          data-detail
          class="vw-detail absolute bottom-0 right-0 top-0 z-40 w-[min(440px,92vw)] overflow-y-auto border-l border-paper-edge bg-paper-soft"
        >
          <button
            type="button"
            aria-label="Close detail"
            class="absolute right-3 top-3 z-10 flex h-7 w-7 items-center justify-center rounded-md bg-paper-mute font-mono text-ink-mute hover:text-ink"
            onclick={closeDetail}
          >
            ✕
          </button>
          <Detail {model} selected={state.selected} />
        </aside>
      {/if}
    </main>
  </div>
</div>

<style>
  /* structural-only; all colors come from Rokkit tokens in the markup */
  .vw-logo :global(svg) {
    width: 22px;
    height: 22px;
    display: block;
  }
  /* hairline divider between segmented buttons (uses the edge token) */
  .vw-seg > button + button {
    border-left: 1px solid var(--paper-edge);
  }
</style>
