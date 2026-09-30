<script lang="ts">
  /**
   * The ER diagram, rendered by `@rokkit/graph`.
   *
   * This was ~140 lines of layout wiring, card markup and edge SVG over a local copy of the
   * clustering algorithm. All of it now lives in the package, which dbd and sensei share —
   * the whole reason for the extraction. What stays here is the adapter: dbd's `SchemaModel`
   * in, the component's props out.
   *
   * The props are unchanged so every call site is untouched.
   */
  import { Graph } from '@rokkit/graph';
  import { toGraphInput } from '@rokkit/graph/schema';
  import { vibe } from '@rokkit/states';
  import type { SchemaModel } from '$lib/design/model';
  // From the package now, not the local copy — the local layout is deleted.
  import type { Density, Arrange, EdgeStyle } from '@rokkit/graph';

  let {
    model,
    density = 'keys',
    arrange = 'untangle',
    lineStyle = 'curved',
    selected = null,
    onSelect,
  }: {
    model: SchemaModel;
    density?: Density;
    arrange?: Arrange;
    lineStyle?: EdgeStyle;
    selected?: string | null;
    onSelect?: (key: string | null) => void;
  } = $props();

  // `er` is the ER half of the model: tables and foreign keys. Views, routines and triggers
  // live in the v2 `entities`/`deps` half — a view is a derived projection and a routine is
  // behaviour, so neither is an entity and neither belongs on this canvas.
  const input = $derived(toGraphInput(model, 'er'));

  // Without this the group ramp resolves from the LIGHT ladder forever, so every cluster
  // keeps a pale fill in dark mode and the canvas reads as light whatever the page is set to.
  // Nothing infers the mode.
  const mode = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
</script>

<Graph
  nodes={input.nodes}
  edges={input.edges}
  fields={input.fields}
  {density}
  {arrange}
  edgeStyle={lineStyle}
  {mode}
  value={selected}
  onselect={(id) => onSelect?.(id)}
  label="{model.project.name} entity relationship diagram"
/>
