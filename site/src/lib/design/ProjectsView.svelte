<script lang="ts">
  import Icon from './Icon.svelte';
  import SchemaSnapshot from './SchemaSnapshot.svelte';
  import type { SavedDiagram } from './store';
  import type { SchemaModel } from './model';

  // The three landing layouts from docs/mockup/designs/projects-page.jsx, driven
  // by the locally-saved diagrams. Each item links to /diagram#<payload>.
  let {
    list,
    layout = 'cards',
    onDelete,
  }: {
    list: SavedDiagram[];
    layout?: 'cards' | 'rows' | 'gallery';
    onDelete?: (project: string) => void;
  } = $props();

  const enumsOf = (m: SchemaModel) => m.schemas.reduce((a, s) => a + s.enums, 0);
  const pl = (n: number, w: string) => `${n} ${w}${n === 1 ? '' : 's'}`;
  const href = (d: SavedDiagram) => '/diagram#' + d.payload;
</script>

{#snippet meta(m: SchemaModel)}
  <span class="ds-badge ds-badge-accent">{m.project.db}</span>
  <span class="ds-badge">{pl(m.schemas.length, 'schema')}</span>
  <span class="ds-badge">{pl(m.tables.length, 'table')}</span>
  {#if enumsOf(m) > 0}<span class="ds-badge">{pl(enumsOf(m), 'enum')}</span>{/if}
  <span class="ds-badge">{pl(m.refs.length, 'ref')}</span>
{/snippet}

{#snippet del(project: string)}
  <button
    type="button"
    class="ds-iconbtn absolute right-2.5 top-2.5 z-10"
    title="Remove from this browser"
    onclick={(e) => {
      e.preventDefault();
      e.stopPropagation();
      onDelete?.(project);
    }}
  >
    <Icon name="x" size={15} />
  </button>
{/snippet}

{#if layout === 'rows'}
  <div class="ds-card overflow-hidden">
    <div
      class="grid grid-cols-[1fr_auto] items-center gap-3 border-b border-line bg-paper-2 px-5 py-2.5 font-mono text-xs uppercase tracking-wider text-faint sm:grid-cols-[2fr_1fr_auto]"
    >
      <span>Design</span>
      <span class="hidden sm:block">Entities</span>
      <span>Engine</span>
    </div>
    {#each list as d, i (d.project)}
      <div class="relative {i > 0 ? 'border-t border-line-soft' : ''}">
        <a
          href={href(d)}
          class="grid grid-cols-[1fr_auto] items-center gap-3 px-5 py-3.5 pr-12 hover:bg-paper-2 sm:grid-cols-[2fr_1fr_auto]"
        >
          <div class="flex min-w-0 items-center gap-3">
            <div
              class="flex h-8 w-8 flex-none items-center justify-center rounded-app-sm border border-line bg-bg-deep font-display text-xs font-semibold text-accent-2"
            >
              {d.project.slice(0, 2)}
            </div>
            <div class="min-w-0">
              <div class="truncate text-sm font-semibold text-fg">{d.project}</div>
              {#if d.model.project.note}<div class="truncate text-xs text-muted">{d.model.project.note}</div>{/if}
            </div>
          </div>
          <div class="hidden font-mono text-xs text-muted sm:block">
            {d.model.tables.length} tables · {d.model.schemas.length} schemas
          </div>
          <span class="ds-badge">{d.model.project.db}</span>
        </a>
        {@render del(d.project)}
      </div>
    {/each}
  </div>
{:else if layout === 'gallery'}
  <div class="grid gap-5 sm:grid-cols-2">
    {#each list as d (d.project)}
      <div class="relative">
        <a href={href(d)} class="ds-card block overflow-hidden">
          <div class="dg-dots border-b border-line bg-bg-deep">
            <SchemaSnapshot schemas={d.model.schemas} class="block h-36 w-full" />
          </div>
          <div class="p-5">
            <span class="font-display text-base font-semibold">{d.project}</span>
            {#if d.model.project.note}<p class="mt-1 text-sm text-muted">{d.model.project.note}</p>{/if}
            <div class="mt-3.5 flex flex-wrap items-center gap-1.5">{@render meta(d.model)}</div>
          </div>
        </a>
        {@render del(d.project)}
      </div>
    {/each}
  </div>
{:else}
  <div class="grid gap-4 sm:grid-cols-2">
    {#each list as d (d.project)}
      <div class="relative">
        <a href={href(d)} class="ds-card block p-5">
          <div class="flex items-start gap-3">
            <div
              class="flex h-10 w-10 flex-none items-center justify-center rounded-app border border-line bg-bg-deep font-display text-sm font-semibold text-accent-2"
            >
              {d.project.slice(0, 2)}
            </div>
            <div class="min-w-0">
              <span class="block truncate font-display text-base font-semibold">{d.project}</span>
              {#if d.model.project.note}<p class="mt-0.5 truncate text-sm text-muted">{d.model.project.note}</p>{/if}
            </div>
          </div>
          <div class="mt-4 flex flex-wrap items-center gap-1.5">{@render meta(d.model)}</div>
          <div class="mt-4 flex items-center gap-2 border-t border-line-soft pt-3 text-xs text-faint">
            <Icon name="terminal" size={12} />
            <span>stored locally</span>
          </div>
        </a>
        {@render del(d.project)}
      </div>
    {/each}
  </div>
{/if}
