<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import { toLayoutData, neighborsOf, type SchemaModel } from '$lib/design/model';
  import { compute, edgePath, type Density, type Arrange, type EdgeStyle } from '$lib/design/layout';

  let {
    model,
    density = 'keys',
    arrange = 'untangle',
    lineStyle = 'curved',
    tint = true,
    selected = null,
    onSelect,
  }: {
    model: SchemaModel;
    density?: Density;
    arrange?: Arrange;
    lineStyle?: EdgeStyle;
    tint?: boolean;
    selected?: string | null;
    onSelect?: (key: string | null) => void;
  } = $props();

  const layout = $derived(compute(toLayoutData(model), density, arrange));
  const related = $derived(selected ? neighborsOf(model, selected) : null);

  // Actual content extent. layout.size adds a +60 margin on the right/bottom
  // only, which makes a fit-to-`size` render hug the left/top edge and float
  // away from the right/bottom. Center on the true content bounds instead so
  // the canvas gutter is symmetric.
  const content = $derived.by(() => {
    let w = 0;
    let h = 0;
    for (const c of layout.clusters) {
      w = Math.max(w, c.x + (c.w ?? 0));
      h = Math.max(h, c.y + (c.h ?? 0));
    }
    return { w: w || layout.size.w, h: h || layout.size.h };
  });

  // Static fit-to-container (no pan/zoom — this is a gallery render).
  const PAD = 28;
  let vw = $state(0);
  let vh = $state(0);
  const scale = $derived(
    Math.max(0.08, Math.min((vw - PAD * 2) / content.w, (vh - PAD * 2) / content.h, 1)) || 0.5
  );
  const tx = $derived((vw - content.w * scale) / 2);
  const ty = $derived((vh - content.h * scale) / 2);

  function cardState(key: string): string {
    if (!selected) return '';
    if (key === selected) return 'sel';
    if (related?.has(key)) return 'rel';
    return 'dim';
  }
  function edgeClass(fromKey: string, toKey: string): string {
    if (!selected) return 'dg-edge';
    return fromKey === selected || toKey === selected ? 'dg-edge hl' : 'dg-edge dim';
  }
</script>

<!-- The viewport fills its nearest positioned ancestor (.dg-viewport is
     position:absolute; inset:0). Place it inside a `relative` box with a height. -->
<div
  class="dg-viewport dg-dots {tint ? 'tinted' : ''}"
  bind:clientWidth={vw}
  bind:clientHeight={vh}
  role="presentation"
  onclick={() => onSelect?.(null)}
>
  <div
    class="dg-world"
    style="width: {layout.size.w}px; height: {layout.size.h}px; transform: translate({tx}px, {ty}px) scale({scale});"
  >
    {#each layout.clusters as c (c.name)}
      <div class="dg-cluster" style="left: {c.x}px; top: {c.y}px; width: {c.w}px; height: {c.h}px; --cl-h: {c.hue};">
        <span class="dg-cluster-label">{c.name} · {c.count}</span>
      </div>
    {/each}

    <svg width={layout.size.w} height={layout.size.h} style="position: absolute; top: 0; left: 0; pointer-events: none;">
      {#each layout.edges as e (e.i)}
        <g class={edgeClass(e.fromKey, e.toKey)}>
          <path d={edgePath(e, lineStyle)} />
          <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
          <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
        </g>
      {/each}
    </svg>

    {#each Object.entries(layout.cards) as [key, card] (key)}
      {@const state = cardState(key)}
      <button
        type="button"
        data-card={key}
        class="dg-card {card.vis.length === 0 && card.more <= 0 ? 'headonly' : ''} {state}"
        style="left: {card.x}px; top: {card.y}px; width: {card.w}px; --cl-h: {card.hue};"
        onclick={(ev) => {
          ev.stopPropagation();
          onSelect?.(key);
        }}
      >
        <div class="dg-card-head">
          <Icon name="table" size={13} class="text-faint" />
          <span class="dg-card-title">{card.t.name}</span>
          <span class="ml-auto font-mono text-faint" style="font-size: 0.62rem;">{card.t.columns.length}</span>
        </div>
        {#each card.vis as col (col.name)}
          <div class="dg-row {col.pk || col.fk ? 'iskey' : ''}">
            {#if col.pk}
              <Icon name="key" size={11} class="dg-keyicon" />
            {:else if col.fk}
              <Icon name="link" size={11} class="dg-fkicon" />
            {:else}
              <span style="width: 11px; flex: none;"></span>
            {/if}
            <span class="cname">{col.name}</span>
            <span class="ctype">{col.type}</span>
          </div>
        {/each}
        {#if card.more > 0}
          <div class="dg-more">+ {card.more} more</div>
        {/if}
      </button>
    {/each}
  </div>
</div>

<style>
  /* The cards are <button>s for a11y; strip native button chrome so they
     render exactly like the design's div cards. */
  .dg-card {
    font: inherit;
    text-align: left;
    color: inherit;
    padding: 0;
  }
</style>
