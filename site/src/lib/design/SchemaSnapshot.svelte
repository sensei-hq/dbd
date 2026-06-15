<script lang="ts">
  import type { SchemaModel } from './model';

  // Zoomed-out schema map: one tinted tile per schema with its table count and
  // a tidy grid of entity blocks. Ported from docs/mockup/designs/app-shell.jsx
  // (SchemaSnapshotThumb). Reads schema name + table count.
  let { schemas, class: cls = '' }: { schemas: SchemaModel['schemas']; class?: string } = $props();

  const HUES = [245, 160, 70, 330, 200, 25, 120, 285];
  const CELL_W = 26, CELL_H = 14, GAP = 5, PAD = 10, LABEL = 15, TILE_GAP = 18, M = 16;

  type Tile = {
    name: string; n: number; ncols: number; hue: number;
    w: number; h: number; x: number; y: number;
    cells: { cx: number; cy: number }[];
  };

  const layout = $derived.by(() => {
    const list = schemas.length ? schemas : [{ name: 'public', tables: 1, enums: 0 }];
    const tiles: Tile[] = list.map((s, i) => {
      const n = Math.max(1, s.tables);
      const ncols = Math.max(1, Math.min(7, Math.round(Math.sqrt(n * 1.7))));
      const nrows = Math.ceil(n / ncols);
      return {
        name: s.name, n, ncols, hue: HUES[i % HUES.length],
        w: ncols * CELL_W + (ncols - 1) * GAP + PAD * 2,
        h: LABEL + nrows * CELL_H + (nrows - 1) * GAP + PAD * 2,
        x: 0, y: 0, cells: [],
      };
    });
    const totalArea = tiles.reduce((a, t) => a + (t.w + TILE_GAP) * (t.h + TILE_GAP), 0);
    const maxW = Math.max(Math.sqrt(totalArea * 2.6), ...tiles.map((t) => t.w));
    let x = 0, y = 0, rowH = 0;
    for (const t of tiles) {
      if (x > 0 && x + t.w > maxW) { x = 0; y += rowH + TILE_GAP; rowH = 0; }
      t.x = x; t.y = y;
      x += t.w + TILE_GAP;
      rowH = Math.max(rowH, t.h);
    }
    for (const t of tiles) {
      t.cells = Array.from({ length: t.n }, (_, i) => ({
        cx: t.x + PAD + (i % t.ncols) * (CELL_W + GAP),
        cy: t.y + PAD + LABEL + Math.floor(i / t.ncols) * (CELL_H + GAP),
      }));
    }
    const W = Math.max(...tiles.map((t) => t.x + t.w));
    const H = y + rowH;
    return { tiles, W, H };
  });
</script>

<svg
  viewBox="{-M} {-M} {layout.W + M * 2} {layout.H + M * 2}"
  class={cls}
  preserveAspectRatio="xMidYMid meet"
  aria-hidden="true"
>
  {#each layout.tiles as t (t.name)}
    <g style="--cl-h: {t.hue};">
      <rect class="sn-tile" x={t.x} y={t.y} width={t.w} height={t.h} rx="6" stroke-dasharray="5 4" stroke-width="1.2" />
      <text class="sn-label" x={t.x + PAD} y={t.y + PAD + 6} font-size="10">{t.name} · {t.n}</text>
      {#each t.cells as cell, i (i)}
        <rect class="sn-cell" x={cell.cx} y={cell.cy} width={CELL_W} height={CELL_H} rx="2.5" stroke-width="1" />
        <rect class="sn-bar" x={cell.cx} y={cell.cy} width={CELL_W} height="4.5" rx="2.5" />
      {/each}
    </g>
  {/each}
</svg>
