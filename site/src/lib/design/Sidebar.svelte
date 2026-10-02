<script lang="ts">
  /**
   * Every entity, by schema (#34) — tables, views, routines, triggers and enums, each in its
   * diagram icon — with a filter by entity type and a search within the kinds that are on.
   */
  import Icon from '$lib/design/Icon.svelte';
  import { allEntities, kindCounts, kindLabel, KIND_ICON, type Kind } from './entities';
  import type { SidebarData } from './data';

  let {
    data,
    selectedKey = null,
    onPick,
  }: {
    data: SidebarData;
    selectedKey?: string | null;
    onPick?: (key: string) => void;
  } = $props();

  let query = $state('');
  // Per-schema collapse override; absent = open.
  let collapsed = $state<Record<string, boolean>>({});
  // Kinds switched off. Empty = every kind shown, which is the default.
  let hidden = $state<Record<string, boolean>>({});

  const q = $derived(query.trim().toLowerCase());
  const kinds = $derived(kindCounts(data.model));

  const groups = $derived.by(() => {
    const shown = allEntities(data.model).filter(
      (e) => !hidden[e.kind] && (!q || e.name.toLowerCase().includes(q)),
    );
    const schemas = [...new Set(shown.map((e) => e.schema))];
    return schemas.map((schema) => ({ schema, items: shown.filter((e) => e.schema === schema) }));
  });

  const isOpen = (name: string) => !collapsed[name];
  const toggle = (kind: Kind) => (hidden[kind] = !hidden[kind]);
</script>

<aside
  class="flex h-full min-h-0 flex-col border-r border-line bg-paper"
  style="width: var(--sb-w);"
>
  <!-- header: project name -->
  <button
    type="button"
    title="Project overview"
    class="border-b border-line-soft px-4 py-3 text-left transition-colors hover:bg-paper-2 {selectedKey
      ? ''
      : 'bg-accent-soft'}"
    onclick={() => onPick?.('')}
  >
    <span
      class="font-display text-sm font-semibold uppercase {selectedKey ? '' : 'text-accent-2'}"
      style="letter-spacing: 0.13em;"
    >
      {data.project.name}
    </span>
  </button>

  <!-- search -->
  <div class="relative p-3 pb-2">
    <span class="pointer-events-none absolute left-6 top-1/2 -translate-y-1/2 text-faint" style="margin-top:1px;">
      <Icon name="search" size={13} />
    </span>
    <input
      class="ds-input py-2 text-sm"
      style="padding-left: 2rem;"
      placeholder="Find an entity…"
      aria-label="Find an entity"
      bind:value={query}
    />
  </div>

  <!-- filter by entity type: one toggle per kind the project has -->
  {#if kinds.length > 1}
    <div class="flex flex-wrap gap-1 px-3 pb-2" role="group" aria-label="Show entity types">
      {#each kinds as { kind, count } (kind)}
        <button
          type="button"
          data-kind-filter={kind}
          aria-pressed={!hidden[kind]}
          class="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 font-mono text-[0.68rem] transition-colors {hidden[
            kind
          ]
            ? 'border-line-soft text-faint'
            : 'border-line bg-accent-soft text-accent-2'}"
          onclick={() => toggle(kind)}
        >
          <span class="{KIND_ICON[kind]} text-[0.8rem]" aria-hidden="true"></span>
          <span>{kindLabel(kind, count)}</span>
          <span class="opacity-70">{count}</span>
        </button>
      {/each}
    </div>
  {/if}

  <!-- grouped list -->
  <div class="ds-scroll min-h-0 flex-1 overflow-y-auto px-2 pb-4">
    {#each groups as { schema, items } (schema)}
      <div class="mt-1" data-schema-group={schema}>
        <button type="button" class="tree-group-head" onclick={() => (collapsed[schema] = !collapsed[schema])}>
          <Icon name={isOpen(schema) || q ? 'chevD' : 'chevR'} size={12} class="text-faint" />
          <span>{schema}</span>
          <span class="ml-auto font-normal text-faint">{items.length}</span>
        </button>

        {#if isOpen(schema) || q}
          <div class="flex flex-col">
            {#each items as item (item.kind + ':' + item.key)}
              <button
                type="button"
                data-entity-key={item.key}
                data-kind={item.kind}
                title={item.kind.replace(/_/g, ' ')}
                class="tree-item {selectedKey === item.key ? 'sel' : ''}"
                onclick={() => onPick?.(item.key)}
              >
                <span class="{KIND_ICON[item.kind]} flex-none text-[0.8rem] opacity-60" aria-hidden="true"></span>
                <span class="ti-name">{item.name}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/each}

    {#if groups.length === 0}
      <p class="px-3 py-6 text-center text-xs text-faint">
        {q ? `Nothing matches “${query}”.` : 'Every entity type is switched off.'}
      </p>
    {/if}
  </div>
</aside>
