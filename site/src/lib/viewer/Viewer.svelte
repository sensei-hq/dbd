<script lang="ts">
  import { ThemeSwitcherToggle } from '@rokkit/app';
  import Sidebar from './Sidebar.svelte';
  import Diagram from './Diagram.svelte';
  import Tabs from './Tabs.svelte';
  import EntitiesList from './EntitiesList.svelte';
  import EntityView from './EntityView.svelte';
  import { createViewerState } from './state.svelte';
  import type { SchemaModel } from './model';
  import type { Density, Arrange } from './layout';
  import './styles.css';
  // Inline the logo's SVG markup (Vite resolves `?raw` to the file's text),
  // so it renders inline rather than via an <img src> request.
  import dbdLogo from '../../../static/dbd.svg?raw';

  let { model }: { model: SchemaModel } = $props();
  const state = createViewerState();

  const tableCount = $derived(model.tables.length);
  const enumCount = $derived(model.schemas.reduce((a, s) => a + s.enums, 0));
  const refCount = $derived(model.refs.length);

  const DENSITIES = ['names', 'keys', 'full'] as const satisfies readonly Density[];
  const ARRANGES = ['untangle', 'a-z'] as const satisfies readonly Arrange[];

  // Open an entity page (EntityView); the breadcrumb / project-name button
  // clear the selection to return to the project root. Accepts null so the
  // Diagram's empty-canvas click (onSelect(null)) returns to the root too.
  function nav(key: string | null): void {
    state.selected = key;
    state.mode = key ? 'focus' : 'overview';
  }

  function toProject(): void {
    state.selected = null;
    state.mode = 'overview';
  }

  // Entity name shown in the breadcrumb (schema.name → name).
  const entityName = $derived(state.selected ? state.selected.split('.')[1] : null);
</script>

<div class="flex h-screen flex-col overflow-hidden bg-paper text-ink">
  <!-- app header -->
  <header
    data-viewer-header
    class="flex flex-none items-center gap-3 border-b border-paper-edge bg-paper-soft px-4 lg:px-6"
    style="height:52px"
  >
    <!-- eslint-disable-next-line svelte/no-at-html-tags — inlined trusted local SVG asset -->
    <span class="vw-logo flex-none" aria-hidden="true">{@html dbdLogo}</span>
    <span class="font-display text-sm font-semibold tracking-tight">dbd</span>
    <span class="ds-badge text-xs font-mono">designs</span>

    <!-- breadcrumb -->
    <nav class="ml-2 hidden items-center gap-1.5 text-sm text-ink-mute sm:flex" aria-label="Breadcrumb">
      <span class="text-ink-soft">/</span>
      <button type="button" class="hover:text-ink" onclick={toProject}>
        {model.project.name}
      </button>
      {#if state.selected}
        <span class="text-ink-soft">/</span>
        <span class="font-medium text-ink">{entityName}</span>
      {/if}
    </nav>

    <div class="ml-auto flex items-center">
      <ThemeSwitcherToggle variant="single" />
    </div>
  </header>

  <!-- body -->
  <div class="relative flex min-h-0 flex-1">
    <!-- sidebar -->
    <aside class="flex flex-none flex-col border-r border-paper-edge bg-paper-soft" style="width:272px">
      <button
        type="button"
        title="Project overview"
        class="border-b border-paper-edge px-4 py-3 text-left transition-colors hover:bg-paper-mute {state.selected
          ? ''
          : 'bg-accent-soft'}"
        onclick={toProject}
      >
        <span
          class="font-display text-sm font-semibold uppercase tracking-[0.13em] {state.selected
            ? ''
            : 'text-primary'}"
        >
          {model.project.name}
        </span>
      </button>
      <Sidebar {model} {state} />
    </aside>

    <!-- main -->
    {#if state.selected}
      <EntityView {model} entityKey={state.selected} onNav={nav} />
    {:else}
      <!-- project root -->
      <div class="flex min-h-0 min-w-0 flex-1 flex-col">
        <!-- design header -->
        <div class="border-b border-paper-edge bg-paper-soft">
          <div class="flex flex-wrap items-start gap-x-6 gap-y-2 px-6 pb-3 pt-5">
            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-3">
                <h1 class="font-display text-lg font-semibold tracking-tight">{model.project.name}</h1>
                <span class="ds-badge ds-badge-accent text-xs font-mono">{model.project.db}</span>
              </div>
              {#if model.project.note}
                <p class="mt-1 max-w-2xl text-sm text-ink-mute">{model.project.note}</p>
              {/if}
            </div>
            <div class="ml-auto hidden whitespace-nowrap pt-1 text-xs font-mono text-ink-soft md:block">
              {tableCount} tables · {enumCount} enums · {refCount} refs
            </div>
          </div>

          <!-- tabs (left) + controls cluster (right), sharing the border -->
          <div class="flex flex-wrap items-center justify-between gap-y-1 pr-6">
            <Tabs
              tabs={[
                ['diagram', 'Diagram', 'grid'],
                ['entities', 'Entities', 'rows'],
              ]}
              active={state.tab}
              onChange={(t) => (state.tab = t as 'diagram' | 'entities')}
            />
            <div class="flex items-center gap-3 py-1.5 text-xs font-mono">
              <!-- density -->
              <div class="vw-seg flex overflow-hidden rounded-md border border-paper-edge">
                {#each DENSITIES as d (d)}
                  <button
                    type="button"
                    data-density={d}
                    aria-pressed={state.density === d}
                    class="px-2 py-1 {state.density === d
                      ? 'bg-primary text-on-primary'
                      : 'bg-paper-mute text-ink-mute'}"
                    onclick={() => (state.density = d)}
                  >
                    {d}
                  </button>
                {/each}
              </div>

              <!-- arrange -->
              <div class="vw-seg flex overflow-hidden rounded-md border border-paper-edge">
                {#each ARRANGES as a (a)}
                  <button
                    type="button"
                    data-arrange={a}
                    aria-pressed={state.arrange === a}
                    class="px-2 py-1 {state.arrange === a
                      ? 'bg-primary text-on-primary'
                      : 'bg-paper-mute text-ink-mute'}"
                    onclick={() => (state.arrange = a)}
                  >
                    {a}
                  </button>
                {/each}
              </div>

              <!-- tint -->
              <button
                type="button"
                data-tint
                aria-pressed={state.tint}
                class="rounded-md border border-paper-edge px-2 py-1 {state.tint
                  ? 'bg-primary text-on-primary'
                  : 'bg-paper-mute text-ink-mute'}"
                onclick={() => (state.tint = !state.tint)}
              >
                tint
              </button>
            </div>
          </div>
        </div>

        <!-- content -->
        {#if state.tab === 'entities'}
          <EntitiesList {model} onNav={nav} />
        {:else}
          <div class="relative min-h-0 min-w-0 flex-1">
            <Diagram {model} {state} onSelect={nav} />
            <div
              class="pointer-events-none absolute bottom-5 left-1/2 z-20 -translate-x-1/2 whitespace-nowrap rounded-full border border-paper-edge bg-paper-soft px-4 py-2 text-xs text-ink-soft shadow-sm"
            >
              drag to pan · ctrl+scroll to zoom · click a table to open it
            </div>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  /* structural-only; all colors come from Rokkit tokens in the markup */
  .vw-logo :global(svg) {
    width: 24px;
    height: 24px;
    display: block;
  }
  /* hairline divider between segmented buttons (uses the edge token) */
  .vw-seg > button + button {
    border-left: 1px solid var(--paper-edge);
  }
</style>
