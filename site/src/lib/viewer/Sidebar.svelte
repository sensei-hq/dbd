<script lang="ts">
  import { List } from '@rokkit/ui';
  import type { SchemaModel } from './model';
  import type { ViewerState } from './state.svelte';

  // The prop is named `state` (public API), but we alias it to `viewer`
  // locally: a local binding called `state` would clash with the `$state`
  // rune (Svelte would read `$state(...)` as a store subscription).
  let { model, state: viewer }: { model: SchemaModel; state: ViewerState } = $props();

  const groups = $derived(
    model.schemas.map((s) => ({
      label: s.name,
      tables: s.tables,
      enums: s.enums,
      children: model.tables
        .filter((t) => t.schema === s.name)
        .map((t) => ({ label: t.name, value: `${s.name}.${t.name}` })),
    }))
  );

  // case-insensitive substring filter on table name; drop emptied groups
  const items = $derived.by(() => {
    const q = viewer.filter.trim().toLowerCase();
    if (!q) return groups;
    return groups
      .map((g) => ({ ...g, children: g.children.filter((c) => c.label.toLowerCase().includes(q)) }))
      .filter((g) => g.children.length > 0);
  });

  function handleSelect(value: unknown) {
    viewer.selected = value as string;
    viewer.mode = 'focus';
  }
</script>

<aside class="vw-sidebar h-full overflow-y-auto bg-paper-soft border-r border-paper-edge text-ink">
  <div class="p-3">
    <input
      data-filter
      type="text"
      aria-label="Filter tables"
      placeholder="Filter tables…"
      bind:value={viewer.filter}
      class="w-full rounded-md bg-paper border border-paper-edge px-2 py-1.5 text-sm text-ink placeholder:text-ink-soft focus:border-primary focus:outline-none"
    />
  </div>

  <List
    {items}
    fields={{ label: 'label', children: 'children', value: 'value' }}
    value={viewer.selected}
    collapsible={false}
    onselect={handleSelect}
  >
    {#snippet groupContent(proxy)}
      <span class="font-mono text-ink">{proxy.label}</span>
      <span class="ml-auto flex items-center gap-1">
        <span class="rounded-full bg-accent-soft px-2 text-primary font-mono text-[0.62rem]">
          {proxy.get('tables')} tables
        </span>
        <span class="rounded-full bg-accent-soft px-2 text-primary font-mono text-[0.62rem]">
          {proxy.get('enums')} enums
        </span>
      </span>
    {/snippet}

    {#snippet itemContent(proxy)}
      <span class="truncate font-mono">{proxy.label}</span>
    {/snippet}
  </List>
</aside>
