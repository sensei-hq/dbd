<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import { nodeId, type Column, type Ref, type SchemaModel, type Table } from '$lib/design/model';

  // Entity-centric diagram: the selected table centered, incoming neighbors on
  // the left, outgoing on the right, with relationship edges. Ported from
  // docs/mockup/designs/entity-page.jsx (EntityDiagram).
  let {
    model,
    entityKey,
    onNav,
  }: { model: SchemaModel; entityKey: string; onNav: (key: string) => void } = $props();

  const C = { CARD_W: 248, ROW_H: 24, HEAD_H: 40, MORE_H: 22, PAD_B: 6, GAP_Y: 26, COL_GAP: 170 };

  // Columns that are the `from` side of any ref → render the link (FK) icon.
  const fkSet = $derived(new Set(model.refs.map((r) => `${r.from.s}.${r.from.t}.${r.from.c}`)));
  const isFk = (t: Table, c: Column) => fkSet.has(`${t.schema}.${t.name}.${c.name}`);

  type Card = { t: Table; vis: Column[]; more: number; w: number; h: number };
  function buildCard(t: Table, mode: 'center' | 'nb', focus: string[] = []): Card {
    let vis: Column[];
    if (mode === 'center') vis = t.columns.slice(0, 16);
    else {
      const want = new Set(focus);
      vis = t.columns.filter((c) => c.pk || want.has(c.name)).slice(0, 8);
    }
    const more = t.columns.length - vis.length;
    const h =
      C.HEAD_H + vis.length * C.ROW_H + (more > 0 ? C.MORE_H : 0) + (vis.length || more > 0 ? C.PAD_B : 0);
    return { t, vis, more, w: C.CARD_W, h };
  }
  function anchorY(card: Card, top: number, colName: string): number {
    const idx = card.vis.findIndex((c) => c.name === colName);
    return idx >= 0 ? top + C.HEAD_H + idx * C.ROW_H + C.ROW_H / 2 : top + C.HEAD_H / 2;
  }
  const find = (key: string) => {
    const [s, n] = key.split('.');
    return model.tables.find((t) => t.schema === s && t.name === n) ?? null;
  };

  type Placed = { key: string; card: Card; x: number; y: number };
  type Edge = { x1: number; y1: number; x2: number; y2: number; out: boolean };
  type NB = { key: string; in: Ref[]; out: Ref[] };

  const m = $derived.by(() => {
    const t = find(entityKey);
    if (!t) return null;

    const selfRefs: Ref[] = [];
    const neighbors = new Map<string, NB>();
    for (const r of model.refs) {
      const fk = nodeId(r.from.s, r.from.t);
      const tk = nodeId(r.to.s, r.to.t);
      if (fk === entityKey && tk === entityKey) {
        selfRefs.push(r);
        continue;
      }
      if (fk === entityKey) {
        const nb = neighbors.get(tk) ?? { key: tk, in: [], out: [] };
        nb.out.push(r);
        neighbors.set(tk, nb);
      } else if (tk === entityKey) {
        const nb = neighbors.get(fk) ?? { key: fk, in: [], out: [] };
        nb.in.push(r);
        neighbors.set(fk, nb);
      }
    }

    const right: NB[] = [];
    const left: NB[] = [];
    for (const nb of neighbors.values()) (nb.out.length ? right : left).push(nb);

    const center = buildCard(t, 'center');
    const mk = (nb: NB) => {
      const t2 = find(nb.key);
      const focus: string[] = [];
      nb.in.forEach((r) => focus.push(r.from.c));
      nb.out.forEach((r) => focus.push(r.to.c));
      return { nb, card: t2 ? buildCard(t2, 'nb', focus) : buildCard(t, 'nb', focus) };
    };
    const L = left.map(mk);
    const R = right.map(mk);
    const stackH = (arr: { card: Card }[]) =>
      arr.reduce((a, x) => a + x.card.h + C.GAP_Y, 0) - (arr.length ? C.GAP_Y : 0);
    const H = Math.max(center.h, stackH(L), stackH(R), 120) + 20;
    const hasL = L.length > 0;
    const hasR = R.length > 0 || selfRefs.length > 0;
    const cx = hasL ? C.CARD_W + C.COL_GAP : 0;
    const W = cx + C.CARD_W + (hasR ? C.COL_GAP + C.CARD_W : 0) + (selfRefs.length ? 60 : 0) + 4;
    const cy = (H - center.h) / 2;

    let yy = (H - stackH(L)) / 2;
    const lPos: Placed[] = L.map((x) => {
      const p = { key: x.nb.key, card: x.card, x: 0, y: yy };
      yy += x.card.h + C.GAP_Y;
      return p;
    });
    yy = (H - stackH(R)) / 2;
    const rxx = cx + C.CARD_W + C.COL_GAP;
    const rPos: Placed[] = R.map((x) => {
      const p = { key: x.nb.key, card: x.card, x: rxx, y: yy };
      yy += x.card.h + C.GAP_Y;
      return p;
    });

    const edges: Edge[] = [];
    L.forEach((x, i) => {
      const p = lPos[i];
      for (const r of x.nb.in)
        edges.push({ x1: p.x + C.CARD_W, y1: anchorY(p.card, p.y, r.from.c), x2: cx, y2: anchorY(center, cy, r.to.c), out: false });
    });
    R.forEach((x, i) => {
      const p = rPos[i];
      for (const r of x.nb.out)
        edges.push({ x1: cx + C.CARD_W, y1: anchorY(center, cy, r.from.c), x2: p.x, y2: anchorY(p.card, p.y, r.to.c), out: true });
      for (const r of x.nb.in)
        edges.push({ x1: p.x, y1: anchorY(p.card, p.y, r.from.c), x2: cx + C.CARD_W, y2: anchorY(center, cy, r.to.c), out: false });
    });
    const loops: Edge[] = selfRefs.map((r) => ({
      x1: cx + C.CARD_W,
      y1: anchorY(center, cy, r.from.c),
      x2: cx + C.CARD_W,
      y2: anchorY(center, cy, r.to.c) + (r.from.c === r.to.c ? 16 : 0),
      out: true,
    }));

    return { center, cx, cy, lPos, rPos, edges, loops, W, H, empty: !lPos.length && !rPos.length && !loops.length };
  });

  function path(e: Edge): string {
    const dx = Math.max(40, Math.min(150, Math.abs(e.x2 - e.x1) / 2));
    const s1 = e.x2 >= e.x1 ? 1 : -1;
    return `M ${e.x1} ${e.y1} C ${e.x1 + dx * s1} ${e.y1}, ${e.x2 - dx * s1} ${e.y2}, ${e.x2} ${e.y2}`;
  }
  function loopPath(e: Edge): string {
    return `M ${e.x1} ${e.y1} C ${e.x1 + 52} ${e.y1}, ${e.x2 + 52} ${e.y2}, ${e.x2} ${e.y2}`;
  }

  let vw = $state(0);
  const scale = $derived(m ? Math.min(1, (vw - 16) / m.W) || 1 : 1);
</script>

{#snippet edCard(card: Card, x: number, y: number, center: boolean)}
  <button
    type="button"
    class="dg-card {center ? 'sel' : ''}"
    style="left: {x}px; top: {y}px; width: {card.w}px; cursor: {center ? 'default' : 'pointer'};"
    onclick={() => !center && onNav(nodeId(card.t.schema, card.t.name))}
  >
    <div class="dg-card-head">
      <Icon name="table" size={13} class="text-faint" />
      <span class="dg-card-title">{center ? card.t.name : `${card.t.schema}.${card.t.name}`}</span>
    </div>
    {#each card.vis as c (c.name)}
      <div class="dg-row {c.pk || isFk(card.t, c) ? 'iskey' : ''}">
        {#if c.pk}
          <Icon name="key" size={11} class="dg-keyicon" />
        {:else if isFk(card.t, c)}
          <Icon name="link" size={11} class="dg-fkicon" />
        {:else}
          <span style="width: 11px; flex: none;"></span>
        {/if}
        <span class="cname">{c.name}</span>
        <span class="ctype">{c.type}</span>
      </div>
    {/each}
    {#if card.more > 0}
      <div class="dg-more">+ {card.more} more</div>
    {/if}
  </button>
{/snippet}

<div class="ds-scroll dg-dots min-h-0 min-w-0 flex-1 overflow-auto bg-bg-deep" bind:clientWidth={vw}>
  {#if m && m.empty}
    <div class="flex h-full flex-col items-center justify-center gap-4 py-20 text-center">
      <div style="width: {m.center.w}px; height: {m.center.h}px; position: relative;">
        {@render edCard(m.center, 0, 0, true)}
      </div>
      <p class="text-sm text-faint">No relationships reference this table.</p>
    </div>
  {:else if m}
    <div class="flex justify-center px-6 py-8">
      <div style="width: {m.W * scale}px; height: {m.H * scale}px;">
        <div style="width: {m.W}px; height: {m.H}px; transform: scale({scale}); transform-origin: 0 0; position: relative;">
          <svg width={m.W} height={m.H} style="position: absolute; inset: 0; pointer-events: none;">
            {#each m.edges as e, i (i)}
              <g class="dg-edge {e.out ? 'hl' : ''}">
                <path d={path(e)} />
                <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
                <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
              </g>
            {/each}
            {#each m.loops as e, i (`loop-${i}`)}
              <g class="dg-edge hl">
                <path d={loopPath(e)} />
                <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
                <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
              </g>
            {/each}
          </svg>
          {#each m.lPos as p (p.key)}
            {@render edCard(p.card, p.x, p.y, false)}
          {/each}
          {@render edCard(m.center, m.cx, m.cy, true)}
          {#each m.rPos as p (p.key)}
            {@render edCard(p.card, p.x, p.y, false)}
          {/each}
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .dg-card {
    font: inherit;
    text-align: left;
    color: inherit;
    padding: 0;
  }
</style>
