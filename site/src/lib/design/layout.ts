/* ============================================================
   dbd designs — deterministic ER diagram layout
   Pure function: schema data + density + arrange -> geometry.
   No DOM measuring; card heights are computed from row counts.

   arrange = "untangle" (default): connectivity-aware ordering —
     clusters are chained by inter-schema link weight and tables
     inside each cluster are reordered with barycenter passes so
     related tables sit near each other and edges cross less.
   arrange = "a-z": plain alphabetical, biggest cluster first.
   ============================================================ */

import type { LayoutData, LayoutColumn, Ref } from './model';
export type { LayoutData };

// ---- public types ----
export type Density = 'names' | 'keys' | 'full';
export type Arrange = 'untangle' | 'a-z';
export type EdgeStyle = 'curved' | 'orthogonal';

/** One table's layout descriptor (a "LayoutTable" is just the LayoutData table row) */
export type LayoutTable = LayoutData['tables'][number];

export type Card = {
  t: LayoutTable;
  vis: LayoutColumn[];
  more: number;
  w: number;
  h: number;
  x: number;
  y: number;
  hue?: number;
};

export type Cluster = {
  name: string;
  list: LayoutTable[];
  count: number;
  hue: number;
  x: number;
  y: number;
  w?: number;
  h?: number;
  pos?: { key: string; dx: number; dy: number }[];
};

export type Edge = {
  i: number;
  ref: Ref;
  fromKey: string;
  toKey: string;
  self: boolean;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  s1: number;
  s2: number;
};

export type Layout = {
  clusters: Cluster[];
  cards: Record<string, Card>;
  edges: Edge[];
  size: { w: number; h: number };
  consts: { CARD_W: number; ROW_H: number; HEAD_H: number };
};

// ---- constants ----
const CARD_W = 248, ROW_H = 24, HEAD_H = 40, MORE_H = 22, PAD_B = 6;
const GAP_X = 36, GAP_Y = 30;
const CL_PAD = 26, CL_TITLE = 16, CL_GAP_X = 110, CL_GAP_Y = 110;
const MAX_ROW_W = 2750;
/* per-schema tint hues (oklch hue angles), assigned a-z */
const HUES = [245, 160, 70, 330, 200, 25, 120, 285];

type Cards = Record<string, Card>;
type Size = { w: number; h: number };

function visibleCols(table: LayoutTable, density: Density): LayoutColumn[] {
  if (density === 'names') return [];
  let cols = table.columns;
  if (density === 'keys') cols = cols.filter((c) => c.pk || c.fk);
  return cols.slice(0, density === 'full' ? 14 : 8);
}

/** Group tables by schema name. */
function groupBySchema(data: LayoutData): Record<string, LayoutTable[]> {
  const bySchema: Record<string, LayoutTable[]> = {};
  for (const t of data.tables) (bySchema[t.schema] = bySchema[t.schema] || []).push(t);
  return bySchema;
}

/** Build a card descriptor per table (height derived from visible-row count). */
function buildCards(data: LayoutData, density: Density): Cards {
  const cards: Cards = {};
  for (const t of data.tables) {
    const vis = visibleCols(t, density);
    const more = t.columns.length - vis.length;
    const h = HEAD_H + vis.length * ROW_H + (more > 0 ? MORE_H : 0) + (vis.length || more > 0 ? PAD_B : 0);
    cards[t.schema + '.' + t.name] = { t, vis, more, w: CARD_W, h, x: 0, y: 0 };
  }
  return cards;
}

/** Undirected table adjacency (cross-table refs) for barycenter ordering. */
function buildAdjacency(data: LayoutData): Record<string, string[]> {
  const nbrs: Record<string, string[]> = {};
  for (const r of data.refs) {
    const f = r.from.s + '.' + r.from.t;
    const t = r.to.s + '.' + r.to.t;
    if (f === t) continue;
    (nbrs[f] = nbrs[f] || []).push(t);
    (nbrs[t] = nbrs[t] || []).push(f);
  }
  return nbrs;
}

/** One cluster per schema; hue assigned by alphabetical schema position (stable across arranges). */
function buildClusters(bySchema: Record<string, LayoutTable[]>): Cluster[] {
  const schemaNames = Object.keys(bySchema).sort();
  return schemaNames.map((s, i) => ({
    name: s,
    list: bySchema[s].slice().sort((a, b) => a.name.localeCompare(b.name)),
    count: bySchema[s].length,
    hue: HUES[i % HUES.length],
    x: 0, y: 0,
  }));
}

/** Masonry-pack one cluster from its current list order (sets c.pos/w/h). */
function pack(c: Cluster, cards: Cards): void {
  const n = c.list.length;
  const ncols = Math.max(1, Math.min(6, Math.round(Math.sqrt(n * 1.15))));
  const colH = new Array<number>(ncols).fill(0);
  c.pos = [];
  for (const t of c.list) {
    const card = cards[c.name + '.' + t.name];
    let ci = 0;
    for (let i = 1; i < ncols; i++) if (colH[i] < colH[ci]) ci = i;
    c.pos.push({ key: c.name + '.' + t.name, dx: ci * (CARD_W + GAP_X), dy: colH[ci] });
    colH[ci] += card.h + GAP_Y;
  }
  c.w = ncols * CARD_W + (ncols - 1) * GAP_X + CL_PAD * 2;
  c.h = Math.max(...colH) - GAP_Y + CL_PAD * 2 + CL_TITLE;
}

const byArea = (a: Cluster, b: Cluster) => (b.w ?? 0) * (b.h ?? 0) - (a.w ?? 0) * (a.h ?? 0);

/**
 * Order clusters. 'untangle' greedily chains schemas so heavily-linked ones stay
 * adjacent (seeded by the largest cluster); otherwise sort by area, biggest first.
 */
function orderClusters(clusters: Cluster[], data: LayoutData, arrange: Arrange): Cluster[] {
  if (arrange !== 'untangle' || clusters.length <= 1) {
    return clusters.sort(byArea);
  }
  const links: Record<string, number> = {};
  for (const r of data.refs) {
    if (r.from.s === r.to.s) continue;
    links[r.from.s + '|' + r.to.s] = (links[r.from.s + '|' + r.to.s] || 0) + 1;
    links[r.to.s + '|' + r.from.s] = (links[r.to.s + '|' + r.from.s] || 0) + 1;
  }
  const lk = (a: Cluster, b: Cluster) => links[a.name + '|' + b.name] || 0;
  const left = clusters.slice().sort(byArea);
  const ordered: Cluster[] = [left.shift()!];
  while (left.length) {
    let best = 0, bestScore = -1;
    for (let i = 0; i < left.length; i++) {
      let score = 0;
      for (let j = 0; j < ordered.length; j++) score += lk(left[i], ordered[j]) * (j + 1);
      if (score > bestScore) { bestScore = score; best = i; }
    }
    ordered.push(left.splice(best, 1)[0]);
  }
  return ordered;
}

/** Flow clusters into rows (wrapping at MAX_ROW_W), place each card, return canvas size. */
function flow(clusters: Cluster[], cards: Cards): Size {
  let x = 0, y = 0, rowH = 0;
  for (const c of clusters) {
    if (x > 0 && x + (c.w ?? 0) > MAX_ROW_W) { x = 0; y += rowH + CL_GAP_Y; rowH = 0; }
    c.x = x; c.y = y;
    x += (c.w ?? 0) + CL_GAP_X;
    rowH = Math.max(rowH, c.h ?? 0);
    for (const p of (c.pos ?? [])) {
      const card = cards[p.key];
      card.x = c.x + CL_PAD + p.dx;
      card.y = c.y + CL_PAD + CL_TITLE + p.dy;
      card.hue = c.hue;
    }
  }
  return {
    w: Math.max(...clusters.map((c) => c.x + (c.w ?? 0))) + 60,
    h: y + rowH + 60,
  };
}

/** Barycenter mean of a table's neighbors (its own center when it has none). */
function barycenter(key: string, nbrs: Record<string, string[]>, cards: Cards): number {
  const card = cards[key];
  const ns = nbrs[key];
  if (!ns || !ns.length) return card.y + card.h / 2;
  let sum = 0;
  for (const nk of ns) { const nc = cards[nk]; sum += nc.y + nc.h / 2; }
  return sum / ns.length;
}

/** Two barycenter passes: reorder each cluster's tables toward neighbors, re-pack, re-flow. */
function barycenterPasses(clusters: Cluster[], cards: Cards, nbrs: Record<string, string[]>): Size {
  let size: Size = { w: 0, h: 0 };
  for (let iter = 0; iter < 2; iter++) {
    for (const c of clusters) {
      const score: Record<string, number> = {};
      for (const t of c.list) score[c.name + '.' + t.name] = barycenter(c.name + '.' + t.name, nbrs, cards);
      c.list = c.list.slice().sort((a, b) => score[c.name + '.' + a.name] - score[c.name + '.' + b.name]);
      pack(c, cards);
    }
    size = flow(clusters, cards);
  }
  return size;
}

/** Vertical anchor for a column's edge endpoint on a card. */
function anchorY(card: Card, colName: string): number {
  const idx = card.vis.findIndex((c) => c.name === colName);
  if (idx >= 0) return card.y + HEAD_H + idx * ROW_H + ROW_H / 2;
  return card.y + HEAD_H / 2;
}

/** Geometry for a single ref: a self-loop on the right edge, or a side-routed connector. */
function buildEdge(r: Ref, i: number, cards: Cards): Edge | null {
  const a = cards[r.from.s + '.' + r.from.t];
  const b = cards[r.to.s + '.' + r.to.t];
  if (!a || !b) return null;
  const fromKey = r.from.s + '.' + r.from.t;
  const toKey = r.to.s + '.' + r.to.t;
  const y1 = anchorY(a, r.from.c);
  const y2 = anchorY(b, r.to.c);

  if (a === b) {
    // self reference: loop on the right edge
    return {
      i, ref: r, fromKey, toKey, self: true,
      x1: a.x + a.w, y1, x2: a.x + a.w, y2: y2 === y1 ? y1 + 14 : y2,
      s1: 1, s2: 1,
    };
  }
  let s1: number, s2: number; // 1 = right side, -1 = left side
  if (a.x + a.w + 50 <= b.x) { s1 = 1; s2 = -1; }
  else if (b.x + b.w + 50 <= a.x) { s1 = -1; s2 = 1; }
  else { s1 = 1; s2 = 1; } // stacked: route around the right
  return {
    i, ref: r, fromKey, toKey, self: false,
    x1: s1 === 1 ? a.x + a.w : a.x, y1,
    x2: s2 === 1 ? b.x + b.w : b.x, y2,
    s1, s2,
  };
}

/** Build all edge geometry, skipping refs whose endpoints aren't laid out. */
function buildEdges(data: LayoutData, cards: Cards): Edge[] {
  const edges: Edge[] = [];
  data.refs.forEach((r, i) => {
    const e = buildEdge(r, i, cards);
    if (e) edges.push(e);
  });
  return edges;
}

export function compute(data: LayoutData, density: Density, arrange: Arrange = 'untangle'): Layout {
  const cards = buildCards(data, density);
  const nbrs = buildAdjacency(data);
  let clusters = buildClusters(groupBySchema(data));
  clusters.forEach((c) => pack(c, cards));

  clusters = orderClusters(clusters, data, arrange);

  // Initial flow, then (for untangle) barycenter passes that reorder + re-flow.
  let size = flow(clusters, cards);
  if (arrange === 'untangle') size = barycenterPasses(clusters, cards, nbrs);

  const edges = buildEdges(data, cards);
  return { clusters, cards, edges, size, consts: { CARD_W, ROW_H, HEAD_H } };
}

export function edgePath(e: Edge, style: EdgeStyle): string {
  const { x1, y1, x2, y2, s1, s2 } = e;
  if (e.self) {
    const bow = 46;
    return `M ${x1} ${y1} C ${x1 + bow} ${y1}, ${x2 + bow} ${y2}, ${x2} ${y2}`;
  }
  if (style === 'orthogonal') {
    if (s1 === 1 && s2 === -1) {
      const mid = (x1 + x2) / 2;
      return `M ${x1} ${y1} H ${mid} V ${y2} H ${x2}`;
    }
    if (s1 === -1 && s2 === 1) {
      const mid = (x1 + x2) / 2;
      return `M ${x1} ${y1} H ${mid} V ${y2} H ${x2}`;
    }
    const out = Math.max(x1, x2) + 52;
    return `M ${x1} ${y1} H ${out} V ${y2} H ${x2}`;
  }
  // curved
  if (s1 === s2) {
    const bow = 64;
    return `M ${x1} ${y1} C ${x1 + bow * s1} ${y1}, ${x2 + bow * s2} ${y2}, ${x2} ${y2}`;
  }
  const dx = Math.max(46, Math.min(170, Math.abs(x2 - x1) / 2));
  return `M ${x1} ${y1} C ${x1 + dx * s1} ${y1}, ${x2 + dx * s2} ${y2}, ${x2} ${y2}`;
}
