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
   * It wears the root diagram's style: the schema tint on every card, and no selection
   * highlight. Neither comes from `Neighborhood`'s props, so this owns the state.
   *
   * Props unchanged, so `EntityView` is untouched.
   */
  import { untrack } from 'svelte';
  import { GraphState, Neighborhood } from '@rokkit/graph';
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

  // `Neighborhood` exposes no `groupTint`, and `flow` at the root has it on — without it the
  // schema a table belongs to is invisible here. A supplied state is the one way in: the
  // diagram then merges only the options its own controls drive (focus, depth, edge style)
  // with `apply`, and this merges the data alongside, so neither resets the other.
  const data = () => ({
    nodes: input.nodes,
    edges: input.edges,
    fields: input.fields,
    mode,
    groupTint: true,
  });
  const graph = new GraphState(untrack(data));
  $effect(() => graph.apply(data()));

  // A click is navigation, never a highlight — the root behaves the same way, because a
  // click there replaces it with this view. A selected focus would outline every neighbour
  // as `related` and dim the second ring to 0.3, the ring a reader asked to see. Clicking a
  // neighbour walks to it; clicking the focus goes nowhere, so its selection is dropped.
  // The package emits `null` on a background click, which is not a navigation.
  function onselect(id: string | null) {
    if (!id) return;
    if (id === entityKey) graph.clear();
    else onNav(id);
  }
</script>

<!-- `focus` drives the layout. -->
<Neighborhood
  state={graph}
  focus={entityKey}
  {onselect}
  controls
  label="Relationships for {entityKey}"
/>
