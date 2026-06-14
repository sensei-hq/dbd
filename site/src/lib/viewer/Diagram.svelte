<script lang="ts">
  import './styles.css';
  import Icon from './Icon.svelte';
  import { toLayoutData, neighborsOf, type SchemaModel } from './model';
  import { compute, edgePath, type Edge } from './layout';
  import type { ViewerState } from './state.svelte';

  // The prop is named `state` (public API), but we alias it to `viewer`
  // locally: a local binding called `state` would clash with the `$state`
  // rune (Svelte would read `$state(...)` as a store subscription).
  let {
    model,
    state: viewer,
    onSelect,
  }: {
    model: SchemaModel;
    state: ViewerState;
    onSelect?: (key: string | null) => void;
  } = $props();

  const data = $derived(toLayoutData(model));
  const layout = $derived(compute(data, viewer.density, viewer.arrange));
  const related = $derived(viewer.selected ? neighborsOf(model, viewer.selected) : null);

  // Keys to render. In focus mode with a selection, restrict to the selected
  // card plus its neighbors (used by the Detail mini-ERD); otherwise show all.
  const visibleKeys = $derived(
    viewer.mode === 'focus' && viewer.selected
      ? Object.keys(layout.cards).filter((key) => key === viewer.selected || related?.has(key))
      : Object.keys(layout.cards)
  );
  const visibleSet = $derived(new Set(visibleKeys));

  // In focus mode only draw edges whose endpoints are both visible.
  const visibleEdges = $derived(
    viewer.mode === 'focus' && viewer.selected
      ? layout.edges.filter((e) => visibleSet.has(e.fromKey) && visibleSet.has(e.toKey))
      : layout.edges
  );

  function cardState(key: string): string {
    if (!viewer.selected) return '';
    if (key === viewer.selected) return 'sel';
    if (related?.has(key)) return 'rel';
    return 'dim';
  }

  function cardClass(key: string): string {
    let cls = 'dg-card';
    const card = layout.cards[key];
    if (card && card.vis.length === 0 && card.more <= 0) cls += ' headonly';
    const s = cardState(key);
    if (s) cls += ' ' + s;
    return cls;
  }

  function edgeClass(e: Edge): string {
    if (!viewer.selected) return 'dg-edge';
    return e.fromKey === viewer.selected || e.toKey === viewer.selected
      ? 'dg-edge hl'
      : 'dg-edge dim';
  }

  function select(key: string | null): void {
    if (onSelect) {
      onSelect(key);
    } else {
      viewer.selected = key;
      viewer.mode = key ? 'focus' : 'overview';
    }
  }

  function selectCard(key: string, e: Event): void {
    e.stopPropagation();
    select(key);
  }

  // ---- pan / zoom (mirrors diagram.jsx: plain DOM transform, no d3) ----
  let vpEl: HTMLDivElement;
  let view = $state({ scale: 0.5, tx: 0, ty: 0 });
  let panning = $state(false);

  function fit(): void {
    const el = vpEl;
    if (!el) return;
    const { w, h } = layout.size;
    const cw = el.clientWidth;
    const ch = el.clientHeight;
    // jsdom (and an unmeasured viewport) report 0 — avoid NaN / divide-by-zero.
    const hasSize = cw > 0 && ch > 0 && w > 0 && h > 0;
    if (!hasSize) {
      view = { scale: 0.5, tx: 0, ty: 0 };
      return;
    }
    const scale = Math.max(0.12, Math.min(cw / w, ch / h, 1));
    view = {
      scale,
      tx: (cw - w * scale) / 2,
      ty: (ch - h * scale) / 2,
    };
  }

  function zoomAt(factor: number, cx: number, cy: number): void {
    const scale = Math.max(0.12, Math.min(2, view.scale * factor));
    const k = scale / view.scale;
    view = { scale, tx: cx - k * (cx - view.tx), ty: cy - k * (cy - view.ty) };
  }

  function zoomCenter(factor: number): void {
    const el = vpEl;
    if (!el) return;
    zoomAt(factor, el.clientWidth / 2, el.clientHeight / 2);
  }

  // pointer pan
  let drag: { x: number; y: number; tx: number; ty: number; moved: boolean } | null = null;

  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    drag = { x: e.clientX, y: e.clientY, tx: view.tx, ty: view.ty, moved: false };
    panning = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent): void {
    const d = drag;
    if (!d) return;
    const dx = e.clientX - d.x;
    const dy = e.clientY - d.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) d.moved = true;
    view = { ...view, tx: d.tx + dx, ty: d.ty + dy };
  }

  function onPointerUp(e: PointerEvent): void {
    const d = drag;
    drag = null;
    panning = false;
    // A click on empty viewport (no drag) clears the selection.
    if (d && !d.moved && e.target === e.currentTarget) select(null);
  }

  // wheel: scroll pans, ctrl/cmd+wheel zooms. Attached with {passive:false} so
  // preventDefault can stop the page from scrolling/zooming.
  function onWheel(e: WheelEvent): void {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const rect = vpEl.getBoundingClientRect();
      zoomAt(Math.exp(-e.deltaY * 0.0022), e.clientX - rect.left, e.clientY - rect.top);
    } else {
      view = { ...view, tx: view.tx - e.deltaX, ty: view.ty - e.deltaY };
    }
  }

  // Fit on mount and whenever the layout's dimensions change (density/arrange/
  // model) or focus changes. Reads layout.size + selection/mode so it re-runs on
  // those; never reads `view`, so writing `view` here cannot self-trigger.
  $effect(() => {
    void layout.size.w;
    void layout.size.h;
    void viewer.selected;
    void viewer.mode;
    fit();
  });
</script>

<div
  bind:this={vpEl}
  class="dg-viewport dg-dots"
  class:tinted={viewer.tint}
  class:panning
  role="presentation"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onwheel={onWheel}
>
  <div
    class="dg-world"
    style="width:{layout.size.w}px;height:{layout.size.h}px;transform:translate({view.tx}px,{view.ty}px) scale({view.scale})"
  >
    <!-- 1. cluster regions -->
    {#each layout.clusters as c (c.name)}
      <div
        class="dg-cluster"
        style="left:{c.x}px;top:{c.y}px;width:{c.w}px;height:{c.h}px;--cl-h:{c.hue}"
      >
        <span class="dg-cluster-label">{c.name} · {c.count}</span>
      </div>
    {/each}

    <!-- 2. relationship edges -->
    <svg
      width={layout.size.w}
      height={layout.size.h}
      style="position:absolute;top:0;left:0;pointer-events:none"
    >
      {#each visibleEdges as e (e.i)}
        <g class={edgeClass(e)}>
          <path d={edgePath(e, viewer.lines)} />
          <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
          <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
        </g>
      {/each}
    </svg>

    <!-- 3. table cards -->
    {#each visibleKeys as key (key)}
      {@const card = layout.cards[key]}
      <div
        data-card={key}
        class={cardClass(key)}
        style="left:{card.x}px;top:{card.y}px;width:{card.w}px;--cl-h:{card.hue}"
        role="button"
        tabindex="0"
        onclick={(e) => selectCard(key, e)}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') selectCard(key, e);
        }}
      >
        <div class="dg-card-head">
          <Icon name="table" size={13} class="dg-card-tableicon" />
          <span class="dg-card-title text-sm font-mono font-semibold">{card.t.name}</span>
          <span class="dg-card-count text-xs font-mono">{card.t.columns.length}</span>
        </div>
        {#each card.vis as col (col.name)}
          <div class="dg-row text-xs font-mono {col.pk || col.fk ? 'iskey' : ''}">
            {#if col.pk}
              <Icon name="key" size={11} class="dg-keyicon" />
            {:else if col.fk}
              <Icon name="link" size={11} class="dg-fkicon" />
            {:else}
              <span class="dg-rowspacer"></span>
            {/if}
            <span class="cname">{col.name}</span>
            <span class="ctype">{col.type}</span>
          </div>
        {/each}
        {#if card.more > 0}
          <div class="dg-more text-xs font-mono">+ {card.more} more</div>
        {/if}
      </div>
    {/each}
  </div>

  <!-- zoom toolbar -->
  <div class="dg-tools">
    <button
      type="button"
      title="Zoom in"
      class="flex h-7 w-7 items-center justify-center rounded-md text-ink-mute hover:text-ink"
      onclick={() => zoomCenter(1.25)}
    >
      <Icon name="plus" size={15} />
    </button>
    <button
      type="button"
      title="Zoom out"
      class="flex h-7 w-7 items-center justify-center rounded-md text-ink-mute hover:text-ink"
      onclick={() => zoomCenter(0.8)}
    >
      <Icon name="minus" size={15} />
    </button>
    <button
      type="button"
      title="Fit to view"
      class="flex h-7 w-7 items-center justify-center rounded-md text-ink-mute hover:text-ink"
      onclick={fit}
    >
      <Icon name="fit" size={15} />
    </button>
  </div>
</div>
