<script lang="ts">
  /**
   * The ER diagram, rendered by `@rokkit/graph`'s `ErDiagram`.
   *
   * This was ~140 lines of layout wiring, card markup and edge SVG over a local copy of the
   * clustering algorithm. All of it now lives in the package, which dbd and sensei share —
   * the whole reason for the extraction. What stays here is the adapter: dbd's `SchemaModel`
   * in, the component's props out.
   *
   * `Graph` is a bare canvas as of 1.7 — no density bar, no zoom buttons — so this composes the
   * named diagram that places them. It ranks by reference direction (`flow`) rather than
   * boxing tables by schema (`cluster`): every edge leaves a card's right side and enters the
   * next one's left. With no schema boxes, the schema is the tint on each card and the legend
   * under the canvas keys it. `arrange` went with `cluster` — ranking has no cluster order to
   * arrange.
   */
  import { ErDiagram } from '@rokkit/graph';
  import { toGraphInput } from '@rokkit/graph/schema';
  import { vibe } from '@rokkit/states';
  import { withStubs, type SchemaModel } from '$lib/design/model';
  // From the package now, not the local copy — the local layout is deleted.
  import type { Density, EdgeStyle } from '@rokkit/graph';

  let {
    model,
    density = $bindable('keys'),
    lineStyle = $bindable('curved'),
    selected = null,
    onSelect,
  }: {
    model: SchemaModel;
    density?: Density;
    lineStyle?: EdgeStyle;
    selected?: string | null;
    onSelect?: (key: string | null) => void;
  } = $props();

  // `er` is the ER half of the model: tables and foreign keys, plus the stubs those keys land
  // on outside the model. Views, routines and triggers live in the v2 `entities`/`deps` half — a
  // view is a derived projection and a routine is behaviour, so neither is an entity and neither
  // belongs on this canvas.
  const input = $derived(toGraphInput(withStubs(model), 'er'));

  // Without this the group ramp resolves from the LIGHT ladder forever, so every card keeps
  // a pale tint in dark mode and the canvas reads as light whatever the page is set to.
  // Nothing infers the mode.
  const mode = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
</script>

<ErDiagram
  nodes={input.nodes}
  edges={input.edges}
  fields={input.fields}
  bind:density
  bind:edgeStyle={lineStyle}
  {mode}
  value={selected}
  onselect={(id) => onSelect?.(id)}
  controls
  legend
  label="{model.project.name} entity relationship diagram"
/>
