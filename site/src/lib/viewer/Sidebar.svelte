<script lang="ts">
  import Icon from './Icon.svelte';
  import { nodeId, type SchemaModel } from './model';
  import type { ViewerState } from './state.svelte';

  // The prop is named `state` (public API), but we alias it to `viewer`
  // locally: a local binding called `state` would clash with the `$state`
  // rune (Svelte would read `$state(...)` as a store subscription).
  let { model, state: viewer }: { model: SchemaModel; state: ViewerState } = $props();

  // Per-schema open/closed overrides, keyed by schema name. A schema with no
  // entry defaults to open, so groups start expanded without capturing `model`.
  let collapsed = $state<Record<string, boolean>>({});
  const isOpen = (name: string) => !collapsed[name];

  const q = $derived(viewer.filter.trim().toLowerCase());

  // Build a group per schema: its (filtered) tables. The mockup also renders
  // an `enums` subgroup, but the current SchemaModel carries only an enum
  // COUNT per schema (no enum entities), so that subgroup is omitted here.
  const groups = $derived.by(() => {
    return model.schemas
      .map((s) => {
        let tables = model.tables.filter((t) => t.schema === s.name);
        if (q) tables = tables.filter((t) => t.name.toLowerCase().includes(q));
        return { schema: s, tables };
      })
      .filter((g) => !q || g.tables.length > 0);
  });

  function toggle(name: string) {
    collapsed[name] = !collapsed[name];
  }

  function pick(schema: string, table: string) {
    viewer.selected = nodeId(schema, table);
    viewer.mode = 'focus';
  }
</script>

<aside class="vw-sidebar flex min-h-0 h-full flex-col bg-paper-soft border-r border-paper-edge text-ink">
  <div class="relative p-3 pb-2">
    <span class="pointer-events-none absolute left-6 top-1/2 -translate-y-1/2 text-ink-soft">
      <Icon name="search" size={13} />
    </span>
    <input
      data-filter
      type="text"
      aria-label="Find an entity"
      placeholder="Find an entity…"
      bind:value={viewer.filter}
      class="w-full rounded-md bg-paper border border-paper-edge pl-8 pr-2 py-2 text-sm text-ink placeholder:text-ink-soft focus:border-primary focus:outline-none"
    />
  </div>

  <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-4">
    {#each groups as { schema, tables } (schema.name)}
      <div class="mt-1">
        <button
          data-tree-group
          type="button"
          class="tree-group-head"
          onclick={() => toggle(schema.name)}
        >
          <Icon name={isOpen(schema.name) || q ? 'chevD' : 'chevR'} size={12} class="text-ink-soft" />
          <span class="text-xs font-mono font-semibold">{schema.name}</span>
          <span class="ml-auto text-xs font-mono font-normal text-ink-soft">{tables.length}</span>
        </button>
        {#if isOpen(schema.name) || q}
          <div class="flex flex-col">
            {#each tables as table (table.name)}
              {@const id = nodeId(schema.name, table.name)}
              <button
                data-tree-item
                type="button"
                class="tree-item"
                class:sel={viewer.selected === id}
                onclick={() => pick(schema.name, table.name)}
              >
                <Icon name="table" size={12} class="flex-none opacity-60" />
                <span class="ti-name text-sm">{table.name}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
    {#if groups.length === 0}
      <p class="px-3 py-6 text-center text-xs text-ink-soft">Nothing matches “{viewer.filter}”.</p>
    {/if}
  </div>
</aside>
