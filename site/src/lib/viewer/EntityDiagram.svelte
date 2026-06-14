<script lang="ts" module>
  import { nodeId, type Column, type Ref, type Table, type SchemaModel } from './model';

  // Geometry constants — ports `ED` from docs/mockup/designs/entity-page.jsx.
  const ED = { CARD_W: 248, ROW_H: 24, HEAD_H: 40, MORE_H: 22, PAD_B: 6, GAP_Y: 26, COL_GAP: 170 };

  type EdCardModel = { t: Table; vis: (Column & { fk: boolean })[]; more: number; w: number; h: number };

  // FK columns of a table = columns appearing as `from.c` in any ref from it.
  function fkSetFor(model: SchemaModel, schema: string, name: string): Set<string> {
    const out = new Set<string>();
    for (const r of model.refs) {
      if (r.from.s === schema && r.from.t === name) out.add(r.from.c);
    }
    return out;
  }

  function withFk(t: Table, fks: Set<string>): (Column & { fk: boolean })[] {
    return t.columns.map((c) => ({ ...c, fk: fks.has(c.name) }));
  }

  function edBuildCard(
    t: Table,
    cols: (Column & { fk: boolean })[],
    mode: 'center' | 'nb',
    focusCols: string[]
  ): EdCardModel {
    let vis: (Column & { fk: boolean })[];
    if (mode === 'center') vis = cols.slice(0, 16);
    else {
      const want = new Set(focusCols);
      vis = cols.filter((c) => c.pk || want.has(c.name)).slice(0, 8);
    }
    const more = cols.length - vis.length;
    const h =
      ED.HEAD_H +
      vis.length * ED.ROW_H +
      (more > 0 ? ED.MORE_H : 0) +
      (vis.length || more > 0 ? ED.PAD_B : 0);
    return { t, vis, more, w: ED.CARD_W, h };
  }

  function edAnchorY(card: EdCardModel, top: number, colName: string): number {
    const idx = card.vis.findIndex((c) => c.name === colName);
    return idx >= 0 ? top + ED.HEAD_H + idx * ED.ROW_H + ED.ROW_H / 2 : top + ED.HEAD_H / 2;
  }

  type Placed = { key: string; card: EdCardModel; x: number; y: number; nbIn: Ref[]; nbOut: Ref[] };
  type EdEdge = { x1: number; y1: number; x2: number; y2: number; out: boolean };
  type EdLayout = {
    center: EdCardModel;
    cx: number;
    cy: number;
    lPos: Placed[];
    rPos: Placed[];
    edges: EdEdge[];
    loops: EdEdge[];
    W: number;
    H: number;
  };

  // Build the entity-centric layout — faithful port of EntityDiagram's useMemo.
  export function buildEntityLayout(model: SchemaModel, entityKey: string): EdLayout | null {
    const [schema, name] = entityKey.split('.');
    const t = model.tables.find((x) => x.schema === schema && x.name === name);
    if (!t) return null;

    const selfRefs: Ref[] = [];
    const neighbors = new Map<string, { key: string; in: Ref[]; out: Ref[] }>();
    for (const r of model.refs) {
      const fk = `${r.from.s}.${r.from.t}`;
      const tk = `${r.to.s}.${r.to.t}`;
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

    // outgoing (and mixed) → right; pure incoming → left
    const right: { key: string; in: Ref[]; out: Ref[] }[] = [];
    const left: { key: string; in: Ref[]; out: Ref[] }[] = [];
    for (const nb of neighbors.values()) (nb.out.length ? right : left).push(nb);

    const center = edBuildCard(t, withFk(t, fkSetFor(model, schema, name)), 'center', []);

    const mk = (nb: { key: string; in: Ref[]; out: Ref[] }) => {
      const [s2, n2] = nb.key.split('.');
      const t2 = model.tables.find((x) => x.schema === s2 && x.name === n2);
      const focus: string[] = [];
      nb.in.forEach((r) => focus.push(r.from.c));
      nb.out.forEach((r) => focus.push(r.to.c));
      const card = t2
        ? edBuildCard(t2, withFk(t2, fkSetFor(model, s2, n2)), 'nb', focus)
        : edBuildCard(
            { schema: s2, name: n2, kind: 'table', columns: [] },
            [],
            'nb',
            focus
          );
      return { nb, card };
    };
    const L = left.map(mk);
    const R = right.map(mk);

    const stackH = (arr: { card: EdCardModel }[]) =>
      arr.reduce((a, x) => a + x.card.h + ED.GAP_Y, 0) - (arr.length ? ED.GAP_Y : 0);
    const H = Math.max(center.h, stackH(L), stackH(R), 120) + 20;
    const hasL = L.length > 0;
    const hasR = R.length > 0 || selfRefs.length > 0;
    const cx = hasL ? ED.CARD_W + ED.COL_GAP : 0;
    const W =
      cx + ED.CARD_W + (hasR ? ED.COL_GAP + ED.CARD_W : 0) + (selfRefs.length ? 60 : 0) + 4;
    const cy = (H - center.h) / 2;

    let yy = (H - stackH(L)) / 2;
    const lPos: Placed[] = L.map((x) => {
      const p: Placed = { key: x.nb.key, card: x.card, x: 0, y: yy, nbIn: x.nb.in, nbOut: x.nb.out };
      yy += x.card.h + ED.GAP_Y;
      return p;
    });
    yy = (H - stackH(R)) / 2;
    const rPos: Placed[] = R.map((x) => {
      const p: Placed = {
        key: x.nb.key,
        card: x.card,
        x: cx + ED.CARD_W + ED.COL_GAP,
        y: yy,
        nbIn: x.nb.in,
        nbOut: x.nb.out,
      };
      yy += x.card.h + ED.GAP_Y;
      return p;
    });

    const edges: EdEdge[] = [];
    for (const p of lPos)
      for (const r of p.nbIn)
        edges.push({
          x1: p.x + ED.CARD_W,
          y1: edAnchorY(p.card, p.y, r.from.c),
          x2: cx,
          y2: edAnchorY(center, cy, r.to.c),
          out: false,
        });
    for (const p of rPos) {
      for (const r of p.nbOut)
        edges.push({
          x1: cx + ED.CARD_W,
          y1: edAnchorY(center, cy, r.from.c),
          x2: p.x,
          y2: edAnchorY(p.card, p.y, r.to.c),
          out: true,
        });
      for (const r of p.nbIn)
        edges.push({
          x1: p.x,
          y1: edAnchorY(p.card, p.y, r.from.c),
          x2: cx + ED.CARD_W,
          y2: edAnchorY(center, cy, r.to.c),
          out: false,
        });
    }
    const loops: EdEdge[] = selfRefs.map((r) => ({
      x1: cx + ED.CARD_W,
      y1: edAnchorY(center, cy, r.from.c),
      x2: cx + ED.CARD_W,
      y2: edAnchorY(center, cy, r.to.c) + (r.from.c === r.to.c ? 16 : 0),
      out: true,
    }));

    return { center, cx, cy, lPos, rPos, edges, loops, W, H };
  }

  // Cubic-bezier path between two edge anchors (ports EntityDiagram's `path`).
  export function edPath(e: EdEdge): string {
    const dx = Math.max(40, Math.min(150, Math.abs(e.x2 - e.x1) / 2));
    const s1 = e.x2 >= e.x1 ? 1 : -1;
    return `M ${e.x1} ${e.y1} C ${e.x1 + dx * s1} ${e.y1}, ${e.x2 - dx * s1} ${e.y2}, ${e.x2} ${e.y2}`;
  }

  export function edLoopPath(e: EdEdge): string {
    return `M ${e.x1} ${e.y1} C ${e.x1 + 52} ${e.y1}, ${e.x2 + 52} ${e.y2}, ${e.x2} ${e.y2}`;
  }
</script>

<script lang="ts">
  import './styles.css';
  import Icon from './Icon.svelte';

  // Ports docs/mockup/designs/entity-page.jsx `EntityDiagram`.
  let {
    model,
    entityKey,
    onNav,
  }: {
    model: SchemaModel;
    entityKey: string;
    onNav: (key: string) => void;
  } = $props();

  const layout = $derived(buildEntityLayout(model, entityKey));
  const empty = $derived(
    !layout || (layout.lPos.length === 0 && layout.rPos.length === 0 && layout.loops.length === 0)
  );

  // Scale-to-fit on resize (ports the mockup's setScale via a resize listener).
  let wrapEl: HTMLDivElement | undefined = $state();
  let scale = $state(1);

  $effect(() => {
    const el = wrapEl;
    const l = layout;
    if (!el || !l) return;
    const update = () => {
      scale = Math.min(1, (el.clientWidth - 16) / l.W);
    };
    update();
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  });
</script>

<div
  bind:this={wrapEl}
  class="ds-scroll dg-dots min-h-0 min-w-0 flex-1 overflow-auto bg-paper"
>
  {#if !layout}
    <div class="flex h-full items-center justify-center py-20 text-sm text-ink-soft">
      Entity not found.
    </div>
  {:else if empty}
    <div class="flex h-full flex-col items-center justify-center gap-4 py-20 text-center">
      <div style="width:{ED.CARD_W}px;height:{layout.center.h}px;position:relative">
        {@render centerCard(layout.center, 0, 0)}
      </div>
      <p class="text-sm text-ink-soft">No relationships reference this table.</p>
    </div>
  {:else}
    <div class="flex justify-center px-6 py-8">
      <div style="width:{layout.W * scale}px;height:{layout.H * scale}px">
        <div
          style="width:{layout.W}px;height:{layout.H}px;transform:scale({scale});transform-origin:0 0;position:relative"
        >
          <svg
            width={layout.W}
            height={layout.H}
            style="position:absolute;inset:0;pointer-events:none"
          >
            {#each layout.edges as e, i (i)}
              <g class={'dg-edge' + (e.out ? ' hl' : '')}>
                <path d={edPath(e)} />
                <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
                <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
              </g>
            {/each}
            {#each layout.loops as e, i (i)}
              <g class="dg-edge hl">
                <path d={edLoopPath(e)} />
                <circle class="dot-from" cx={e.x1} cy={e.y1} r="3.2" />
                <circle class="dot-to" cx={e.x2} cy={e.y2} r="3.2" />
              </g>
            {/each}
          </svg>
          {#each layout.lPos as p (p.key)}
            {@render neighborCard(p.card, p.x, p.y)}
          {/each}
          {@render centerCard(layout.center, layout.cx, layout.cy)}
          {#each layout.rPos as p (p.key)}
            {@render neighborCard(p.card, p.x, p.y)}
          {/each}
        </div>
      </div>
    </div>
  {/if}
</div>

<!-- The central entity card: a non-interactive display node (no nav). -->
{#snippet centerCard(card: EdCardModel, x: number, y: number)}
  {@const key = nodeId(card.t.schema, card.t.name)}
  <div
    data-ed-card={key}
    class="dg-card sel"
    style="left:{x}px;top:{y}px;width:{card.w}px;position:absolute;cursor:default"
  >
    <div class="dg-card-head">
      <Icon name="table" size={13} class="dg-card-tableicon" />
      <span class="dg-card-title text-sm font-mono font-semibold">{card.t.name}</span>
    </div>
    {@render cardRows(card)}
  </div>
{/snippet}

<!-- A neighbor card: clickable, navigates to that entity. -->
{#snippet neighborCard(card: EdCardModel, x: number, y: number)}
  {@const key = nodeId(card.t.schema, card.t.name)}
  <div
    data-ed-card={key}
    class="dg-card"
    style="left:{x}px;top:{y}px;width:{card.w}px;position:absolute;cursor:pointer"
    role="button"
    tabindex="0"
    onclick={() => onNav(key)}
    onkeydown={(e) => {
      if (e.key === 'Enter' || e.key === ' ') onNav(key);
    }}
  >
    <div class="dg-card-head">
      <Icon name="table" size={13} class="dg-card-tableicon" />
      <span class="dg-card-title text-sm font-mono font-semibold">{key}</span>
    </div>
    {@render cardRows(card)}
  </div>
{/snippet}

{#snippet cardRows(card: EdCardModel)}
  {#each card.vis as c (c.name)}
    <div class="dg-row text-xs font-mono {c.pk || c.fk ? 'iskey' : ''}">
      {#if c.pk}
        <Icon name="key" size={11} class="dg-keyicon" />
      {:else if c.fk}
        <Icon name="link" size={11} class="dg-fkicon" />
      {:else}
        <span class="dg-rowspacer"></span>
      {/if}
      <span class="cname">{c.name}</span>
      <span class="ctype">{c.type}</span>
    </div>
  {/each}
  {#if card.more > 0}
    <div class="dg-more text-xs font-mono">+ {card.more} more</div>
  {/if}
{/snippet}
