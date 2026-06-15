<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import { nodeId } from '$lib/design/model';
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
  let enumsOpen = $state<Record<string, boolean>>({});

  const q = $derived(query.trim().toLowerCase());

  const groups = $derived.by(() =>
    data.model.schemas
      .map((s) => {
        let tables = data.model.tables.filter((t) => t.schema === s.name);
        let enums = data.enums.filter((e) => e.schema === s.name);
        if (q) {
          tables = tables.filter((t) => t.name.toLowerCase().includes(q));
          enums = enums.filter((e) => e.name.toLowerCase().includes(q));
        }
        return { schema: s, tables, enums };
      })
      .filter((g) => !q || g.tables.length || g.enums.length)
  );

  const isOpen = (name: string) => !collapsed[name];
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

  <!-- grouped list -->
  <div class="ds-scroll min-h-0 flex-1 overflow-y-auto px-2 pb-4">
    {#each groups as { schema, tables, enums } (schema.name)}
      <div class="mt-1">
        <button type="button" class="tree-group-head" onclick={() => (collapsed[schema.name] = !collapsed[schema.name])}>
          <Icon name={isOpen(schema.name) || q ? 'chevD' : 'chevR'} size={12} class="text-faint" />
          <span>{schema.name}</span>
          <span class="ml-auto font-normal text-faint">{tables.length}</span>
        </button>

        {#if isOpen(schema.name) || q}
          <div class="flex flex-col">
            {#each tables as table (table.name)}
              {@const key = nodeId(schema.name, table.name)}
              <button
                type="button"
                class="tree-item {selectedKey === key ? 'sel' : ''}"
                onclick={() => onPick?.(key)}
              >
                <Icon name="table" size={12} class="flex-none opacity-60" />
                <span class="ti-name">{table.name}</span>
              </button>
            {/each}

            {#if enums.length}
              <button
                type="button"
                class="tree-group-head"
                style="padding-left: 26px; color: var(--muted); font-weight: 500;"
                onclick={() => (enumsOpen[schema.name] = !enumsOpen[schema.name])}
              >
                <Icon name={enumsOpen[schema.name] || q ? 'chevD' : 'chevR'} size={11} class="text-faint" />
                <span>enums</span>
                <span class="ml-auto font-normal text-faint">{enums.length}</span>
              </button>
              {#if enumsOpen[schema.name] || q}
                {#each enums as en (en.name)}
                  {@const key = nodeId(schema.name, en.name)}
                  <button
                    type="button"
                    class="tree-item {selectedKey === key ? 'sel' : ''}"
                    style="padding-left: 42px;"
                    onclick={() => onPick?.(key)}
                  >
                    <Icon name="enumI" size={12} class="flex-none opacity-60" />
                    <span class="ti-name">{en.name}</span>
                  </button>
                {/each}
              {/if}
            {/if}
          </div>
        {/if}
      </div>
    {/each}

    {#if groups.length === 0}
      <p class="px-3 py-6 text-center text-xs text-faint">Nothing matches “{query}”.</p>
    {/if}
  </div>
</aside>
