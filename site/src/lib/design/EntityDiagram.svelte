<script lang="ts">
  /**
   * The entity-centric diagram, rendered by `@rokkit/graph`'s `Neighborhood`.
   *
   * Was ~220 lines: a hand-rolled left/right partition, card sizing, anchor maths and edge
   * SVG. All of it is the package's `neighborhood` layout now — the focused node centred,
   * things that reference it stacked left, things it references stacked right.
   *
   * The named diagram places the depth, edge-style and zoom controls. Cards are always at
   * full detail here — a portrait of one table's surroundings that hid the columns its edges
   * land on would defeat its own purpose — so there is no density to pass.
   *
   * It wears the root diagram's style — the schema tint on every card, no selection
   * highlight — which `Neighborhood` does itself since 1.8.1 (rokkit#172). Before that, this
   * component owned the `GraphState` to get there.
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

<!-- `focus` drives the layout. A click is navigation, never a highlight, as at the root:
     clicking a neighbour walks to it, which is how you move through a schema one hop at a
     time. Clicking the focus goes nowhere, and the package emits `null` on a background
     click, which is not a navigation either. -->
<Neighborhood
  nodes={input.nodes}
  edges={input.edges}
  fields={input.fields}
  focus={entityKey}
  {mode}
  onselect={(id) => id && id !== entityKey && onNav(id)}
  controls
  label="Relationships for {entityKey}"
/>
