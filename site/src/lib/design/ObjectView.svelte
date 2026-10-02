<script lang="ts">
  /**
   * A view, materialized view, function, procedure or trigger (#34): what it is, what it uses
   * and what uses it — both from the v2 dependency graph — and its neighbourhood in that graph.
   * These kinds have no columns in the model, so there is no fields table; and snapshots hold
   * tables and enums only, so the changelog says why it is empty.
   */
  import { Neighborhood } from '@rokkit/graph';
  import { toGraphInput } from '@rokkit/graph/schema';
  import { vibe } from '@rokkit/states';
  import Tabs from './Tabs.svelte';
  import Markdown from './Markdown.svelte';
  import EntityChangelog from './EntityChangelog.svelte';
  import { noteBlocks } from './md';
  import { entityAt, KIND_ICON } from './entities';
  import type { DepEdge, SchemaModel } from './model';

  let {
    model,
    entityKey,
    onNav,
  }: { model: SchemaModel; entityKey: string; onNav: (key: string) => void } = $props();

  let tab = $state('details');
  $effect(() => {
    void entityKey;
    tab = 'details';
  });

  const item = $derived(entityAt(model, entityKey));
  const node = $derived(model.entities?.find((e) => `${e.schema}.${e.name}` === entityKey));
  const comment = $derived(noteBlocks(node?.noteMd ?? node?.note));
  const key = (s: string, n: string) => `${s}.${n}`;
  const uses = $derived((model.deps ?? []).filter((d) => key(d.from.s, d.from.n) === entityKey));
  const usedBy = $derived(
    (model.deps ?? []).filter((d) => key(d.to.s, d.to.n) === entityKey && key(d.from.s, d.from.n) !== entityKey),
  );

  const input = $derived(toGraphInput(model, 'dependencies'));
  const mode = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
</script>

{#snippet depRows(edges: DepEdge[], side: 'to' | 'from', empty: string)}
  {#if edges.length}
    <div class="mt-3 flex max-w-4xl flex-col">
      {#each edges as d (d[side].s + '.' + d[side].n + ':' + d.kind)}
        {@const other = entityAt(model, key(d[side].s, d[side].n))}
        <div
          data-dep={key(d[side].s, d[side].n)}
          class="flex items-center gap-3 border-b border-line-soft py-2.5 font-mono text-xs text-muted last:border-0 {d.unresolved
            ? 'opacity-60'
            : ''}"
          title={d.unresolved ? 'Not defined in this project' : undefined}
        >
          {#if other}
            <span class="{KIND_ICON[other.kind]} text-faint" aria-hidden="true"></span>
            <button type="button" class="font-semibold text-accent-2 hover:underline" onclick={() => onNav(other.key)}
              >{other.key}</button
            >
            <span class="col-badge">{other.kind.replace(/_/g, ' ')}</span>
          {:else}
            <span class="font-semibold text-fg">{key(d[side].s, d[side].n)}</span>
          {/if}
          <span class="ml-auto text-faint">{d.kind}</span>
        </div>
      {/each}
    </div>
  {:else}
    <p class="mt-3 text-sm text-faint">{empty}</p>
  {/if}
{/snippet}

{#if item && node}
  <div data-object-view class="flex min-h-0 min-w-0 flex-1 flex-col">
    <div class="border-b border-line bg-paper">
      <div class="px-6 pb-3 pt-5">
        <div class="flex flex-wrap items-center gap-3">
          <div class="flex items-baseline">
            <span class="font-mono text-sm text-faint">{item.schema}.</span>
            <h1 class="font-display text-h3 font-semibold tracking-tight">{item.name}</h1>
          </div>
          <span data-kind-badge class="ds-badge">{item.kind.replace(/_/g, ' ')}</span>
          <span class="ds-badge">{uses.length} uses · {usedBy.length} used by</span>
        </div>
      </div>
      <Tabs
        tabs={[
          { id: 'details', label: 'Details', icon: 'rows' },
          { id: 'diagram', label: 'Diagram', icon: 'grid' },
          { id: 'changelog', label: 'Changelog', icon: 'clock' },
        ]}
        active={tab}
        onChange={(id) => (tab = id)}
      />
    </div>

    {#if tab === 'details'}
      <div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
        <div class="px-6 py-6">
          <section data-section="info">
            <h2 class="font-mono text-label uppercase text-faint">Info</h2>
            <div class="mt-3 flex max-w-3xl flex-col gap-2 text-sm leading-relaxed text-muted">
              {#if comment.length}
                <Markdown blocks={comment} />
              {:else}
                <p class="text-faint">No comment on this {item.kind.replace(/_/g, ' ')}.</p>
              {/if}
            </div>
          </section>
          <section data-section="uses" class="mt-8">
            <h2 class="font-mono text-label uppercase text-faint">Uses <span class="text-faint">· {uses.length}</span></h2>
            {@render depRows(uses, 'to', 'It reads, writes and calls nothing in this project.')}
          </section>
          <section data-section="used-by" class="mt-8 pb-6">
            <h2 class="font-mono text-label uppercase text-faint">
              Used by <span class="text-faint">· {usedBy.length}</span>
            </h2>
            {@render depRows(usedBy, 'from', 'Nothing in this project uses it.')}
          </section>
        </div>
      </div>
    {:else if tab === 'changelog'}
      <EntityChangelog {model} kind={item.kind} schema={item.schema} name={item.name} />
    {:else}
      <!-- The dependency graph, not the ER one: a view's neighbours are what it reads. -->
      <Neighborhood
        nodes={input.nodes}
        edges={input.edges}
        fields={input.fields}
        focus={entityKey}
        {mode}
        onselect={(id) => id && id !== entityKey && onNav(id)}
        controls
        label="Dependencies of {entityKey}"
      />
    {/if}
  </div>
{:else}
  <div class="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-bg text-sm text-muted">Entity not found.</div>
{/if}
