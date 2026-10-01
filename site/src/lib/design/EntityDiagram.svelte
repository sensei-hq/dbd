<script lang="ts">
  /**
   * The entity-centric diagram, rendered by `@rokkit/graph`'s `Neighborhood`.
   *
   * Was ~220 lines: a hand-rolled left/right partition, card sizing, anchor maths and edge
   * SVG. All of it is the package's `neighborhood` layout now — the focused node centred,
   * things that reference it stacked left, things it references stacked right.
   *
   * `Graph` stopped drawing zoom buttons in 1.7; the named diagram places them, with a depth
   * control beside them. Cards are always at full detail here — a portrait of one table's
   * surroundings that hid the columns its edges land on would defeat its own purpose — so
   * there is no density to pass.
   *
   * Props unchanged, so `EntityView` is untouched.
   */
  import { Neighborhood } from '@rokkit/graph';
  import { toGraphInput } from '@rokkit/graph/schema';
  import { vibe } from '@rokkit/states';
  import type { SchemaModel } from '$lib/design/model';

  let {
    model,
    entityKey,
    onNav,
  }: { model: SchemaModel; entityKey: string; onNav: (key: string) => void } = $props();

  const input = $derived(toGraphInput(model, 'er'));
  const mode = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
</script>

<!-- `focus` drives the layout; `value` keeps the focused card marked as selected. Clicking a
     neighbour re-focuses onto it, which is how you walk a schema one hop at a time — the
     package emits `null` on a background click, which is not a navigation. -->
<Neighborhood
  nodes={input.nodes}
  edges={input.edges}
  fields={input.fields}
  focus={entityKey}
  value={entityKey}
  {mode}
  onselect={(id) => id && onNav(id)}
  controls
  label="Relationships for {entityKey}"
/>
