<script lang="ts">
  /**
   * Every entity, by schema (#34) — tables, views, routines, triggers and enums, each in its
   * diagram icon — with a filter by entity type and a search within the kinds that are on. A
   * workspace's model is grouped by module first, in `depends_on` order, then by schema (#7).
   */
  import Icon from '$lib/design/Icon.svelte';
  import { allEntities, kindCounts, kindLabel, KIND_ICON, type EntityItem, type Kind } from './entities';
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
  // Per-group collapse override, keyed `module` or `module/schema` (`/schema` in a project's
  // model); absent = open.
  let collapsed = $state<Record<string, boolean>>({});
  // Kinds switched off. Empty = every kind shown, which is the default.
  let hidden = $state<Record<string, boolean>>({});

  const q = $derived(query.trim().toLowerCase());
  const kinds = $derived(kindCounts(data.model));

  const bySchema = (items: EntityItem[]) =>
    [...new Set(items.map((e) => e.schema))].map((schema) => ({
      schema,
      items: items.filter((e) => e.schema === schema),
    }));

  // A project's model is one unnamed module. A module with nothing left to show is left out,
  // as a schema is; an entity naming no module the model lists is listed after them, unnamed,
  // rather than not at all.
  const groups = $derived.by(() => {
    const shown = allEntities(data.model).filter(
      (e) => !hidden[e.kind] && (!q || e.name.toLowerCase().includes(q)),
    );
    const names = (data.model.modules ?? []).map((m) => m.name);
    const rest = shown.filter((e) => !e.module || !names.includes(e.module));
    return [
      ...names.map((module) => ({ module, schemas: bySchema(shown.filter((e) => e.module === module)) })),
      { module: '', schemas: bySchema(rest) },
    ].filter((m) => m.schemas.length > 0);
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
    {#each groups as { module, schemas } (module)}
      {#if module}
        <div class="mt-2" data-module-group={module}>
          <button type="button" class="tree-group-head" onclick={() => (collapsed[module] = !collapsed[module])}>
            <Icon name={isOpen(module) || q ? 'chevD' : 'chevR'} size={12} class="text-faint" />
            <span class="font-semibold">{module}</span>
            <span class="ml-auto font-normal text-faint">
              {schemas.reduce((n, s) => n + s.items.length, 0)}
            </span>
          </button>
          {#if isOpen(module) || q}
            <div class="pl-2">{@render schemaGroups(module, schemas)}</div>
          {/if}
        </div>
      {:else}
        {@render schemaGroups(module, schemas)}
      {/if}
    {/each}

    {#if groups.length === 0}
      <p class="px-3 py-6 text-center text-xs text-faint">
        {q ? `Nothing matches “${query}”.` : 'Every entity type is switched off.'}
      </p>
    {/if}
  </div>
</aside>

{#snippet schemaGroups(module: string, schemas: { schema: string; items: EntityItem[] }[])}
  {#each schemas as { schema, items } (schema)}
    {@const key = `${module}/${schema}`}
    <div class="mt-1" data-schema-group={schema}>
      <button type="button" class="tree-group-head" onclick={() => (collapsed[key] = !collapsed[key])}>
        <Icon name={isOpen(key) || q ? 'chevD' : 'chevR'} size={12} class="text-faint" />
        <span>{schema}</span>
        <span class="ml-auto font-normal text-faint">{items.length}</span>
      </button>

      {#if isOpen(key) || q}
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
{/snippet}
