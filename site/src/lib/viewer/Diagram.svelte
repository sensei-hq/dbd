<script lang="ts">
  import './styles.css';
  import { onMount } from 'svelte';
  import { select } from 'd3-selection';
  import { zoom, zoomIdentity, type ZoomBehavior } from 'd3-zoom';
  import { toLayoutData, neighborsOf, type SchemaModel } from './model';
  import { compute, edgePath, type Edge } from './layout';
  import type { ViewerState } from './state.svelte';

  // The prop is named `state` (public API), but we alias it to `viewer`
  // locally: a local binding called `state` would clash with the `$state`
  // rune (Svelte would read `$state(...)` as a store subscription).
  let { model, state: viewer }: { model: SchemaModel; state: ViewerState } = $props();

  const data = $derived(toLayoutData(model));
  const layout = $derived(compute(data, viewer.density, viewer.arrange));
  const related = $derived(viewer.selected ? neighborsOf(model, viewer.selected) : null);

  // Keys that should be rendered. In focus mode with a selection, restrict to
  // the selected card plus its neighbors; otherwise show everything.
  const visibleKeys = $derived(
    viewer.mode === 'focus' && viewer.selected
      ? Object.keys(layout.cards).filter((key) => key === viewer.selected || related?.has(key))
      : Object.keys(layout.cards)
  );
  const visibleSet = $derived(new Set(visibleKeys));

  function cardClass(key: string): string {
    let cls = 'dg-card';
    const card = layout.cards[key];
    if (card && card.vis.length === 0 && card.more <= 0) cls += ' headonly';
    if (viewer.selected) {
      if (key === viewer.selected) cls += ' sel';
      else if (related?.has(key)) cls += ' rel';
      else cls += ' dim';
    }
    return cls;
  }

  function edgeClass(e: Edge): string {
    if (!viewer.selected) return 'dg-edge';
    return e.fromKey === viewer.selected || e.toKey === viewer.selected ? 'dg-edge hl' : 'dg-edge dim';
  }

  // In focus mode only draw edges whose endpoints are both visible.
  const visibleEdges = $derived(
    viewer.mode === 'focus' && viewer.selected
      ? layout.edges.filter((e) => visibleSet.has(e.fromKey) && visibleSet.has(e.toKey))
      : layout.edges
  );

  function selectCard(key: string, e: Event) {
    e.stopPropagation();
    viewer.selected = key;
    viewer.mode = 'focus';
  }

  function clearSelection() {
    viewer.selected = null;
    viewer.mode = 'overview';
  }

  // ---- pan / zoom ----
  let svgEl: SVGSVGElement;
  let t = $state({ x: 0, y: 0, k: 0.5 });
  let zb: ZoomBehavior<SVGSVGElement, unknown> | null = null;

  function fit() {
    if (!svgEl || !zb) return;
    const { w, h } = layout.size;
    const cw = svgEl.clientWidth;
    const ch = svgEl.clientHeight;
    // jsdom (and an unmeasured viewport) report 0 — avoid NaN / divide-by-zero.
    const hasSize = cw > 0 && ch > 0 && w > 0 && h > 0;
    const scale = hasSize ? Math.max(0.12, Math.min(cw / w, ch / h, 1)) : 0.5;
    const tx = hasSize ? (cw - w * scale) / 2 : 0;
    const ty = hasSize ? (ch - h * scale) / 2 : 0;
    // Pin a static extent from the measured client box before applying the
    // transform. d3-zoom's default extent reads the SVG's `viewBox`/`width`
    // `baseVal`, which jsdom does not implement (it would throw).
    zb.extent([
      [0, 0],
      [cw || 1, ch || 1],
    ]);
    select(svgEl).call(zb.transform, zoomIdentity.translate(tx, ty).scale(scale));
  }

  onMount(() => {
    zb = zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.12, 2])
      // Static extent (not the default accessor) so d3-zoom never touches the
      // SVG `baseVal` properties jsdom lacks; fit() refreshes it from the box.
      .extent([
        [0, 0],
        [1, 1],
      ])
      .on('zoom', (ev) => {
        t = { x: ev.transform.x, y: ev.transform.y, k: ev.transform.k };
      });
    select(svgEl).call(zb);
    fit();
    // Detach d3-zoom's event listeners on unmount so they don't leak.
    return () => {
      if (svgEl) select(svgEl).on('.zoom', null);
    };
  });

  // Re-fit whenever the computed layout's dimensions change (e.g. toggling
  // density/arrange). Reads layout.size.{w,h} so it re-runs on layout changes,
  // but never reads `t` — fit() writes `t` via the zoom handler, so reading
  // size (not t) here means there is no self-triggering loop. Guarded until the
  // zoom behavior and SVG element are initialized by onMount.
  $effect(() => {
    // Track the layout dimensions so the effect re-runs when they change.
    void layout.size.w;
    void layout.size.h;
    if (!zb || !svgEl) return;
    fit();
  });
</script>

<svg
  bind:this={svgEl}
  class="dg-viewport"
  role="presentation"
  onclick={clearSelection}
>
  <g class="dg-world" transform={`translate(${t.x},${t.y}) scale(${t.k})`}>
    <!-- 1. cluster regions -->
    {#each layout.clusters as c (c.name)}
      <rect
        class="dg-cluster"
        x={c.x}
        y={c.y}
        width={c.w}
        height={c.h}
        style="--cl-h:{c.hue}"
      />
      <text class="dg-cluster-label" x={c.x + 18} y={c.y - 4}>{c.name} · {c.count}</text>
    {/each}

    <!-- 2. edges -->
    {#each visibleEdges as e (e.i)}
      <g class={edgeClass(e)}>
        <path d={edgePath(e, 'curved')} />
        <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
        <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
      </g>
    {/each}

    <!-- 3. table cards -->
    {#each visibleKeys as key (key)}
      {@const card = layout.cards[key]}
      <foreignObject x={card.x} y={card.y} width={card.w} height={card.h}>
        <div
          data-card={key}
          class={cardClass(key)}
          style="--cl-h:{card.hue}"
          role="button"
          tabindex="0"
          onclick={(e) => selectCard(key, e)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') selectCard(key, e);
          }}
        >
          <div class="dg-card-head">
            <span class="dg-card-tableicon" aria-hidden="true">▦</span>
            <span class="dg-card-title font-mono">{card.t.name}</span>
            <span class="dg-card-count font-mono">{card.t.columns.length}</span>
          </div>
          {#each card.vis as col (col.name)}
            <div class="dg-row font-mono {col.pk || col.fk ? 'iskey' : ''}">
              {#if col.pk}
                <span class="dg-keyicon" aria-hidden="true">●</span>
              {:else if col.fk}
                <span class="dg-fkicon" aria-hidden="true">◇</span>
              {:else}
                <span class="dg-rowspacer"></span>
              {/if}
              <span class="cname">{col.name}</span>
              <span class="ctype">{col.type}</span>
            </div>
          {/each}
          {#if card.more > 0}
            <div class="dg-more font-mono">+ {card.more} more</div>
          {/if}
        </div>
      </foreignObject>
    {/each}
  </g>
</svg>
