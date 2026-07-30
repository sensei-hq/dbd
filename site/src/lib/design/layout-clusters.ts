/* ============================================================
   ER diagram layout: cluster building, ordering, packing, flow.
   This is the core of the layout algorithm — schemas become clusters,
   clusters are ordered to minimise edge crossings, and each cluster's
   tables are masonry-packed then flowed into wrapping rows.
   ============================================================ */

import type { LayoutData, LayoutTable, Cluster, Cards, Size, Arrange } from './layout-types';
import {
  CARD_W, GAP_X, GAP_Y, CL_PAD, CL_TITLE, CL_GAP_X, CL_GAP_Y, MAX_ROW_W, HUES,
} from './layout-types';

/** Group tables by schema name. */
export function groupBySchema(data: LayoutData): Record<string, LayoutTable[]> {
  const bySchema: Record<string, LayoutTable[]> = {};
  for (const t of data.tables) (bySchema[t.schema] = bySchema[t.schema] || []).push(t);
  return bySchema;
}

/** Undirected table adjacency (cross-table refs) for barycenter ordering. */
export function buildAdjacency(data: LayoutData): Record<string, string[]> {
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
export function buildClusters(bySchema: Record<string, LayoutTable[]>): Cluster[] {
  const schemaNames = Object.keys(bySchema).sort();
  return schemaNames.map((s, i) => ({
    name: s,
    list: bySchema[s].slice().sort((a, b) => a.name.localeCompare(b.name)),
    count: bySchema[s].length,
    hue: HUES[i % HUES.length],
    x: 0, y: 0,
  }));
}

/** Index of the shortest column so far (first one wins on a tie). */
function shortestColumn(colH: number[]): number {
  let ci = 0;
  for (let i = 1; i < colH.length; i++) if (colH[i] < colH[ci]) ci = i;
  return ci;
}

/** Masonry-pack one cluster from its current list order (sets c.pos/w/h). */
export function pack(c: Cluster, cards: Cards): void {
  const n = c.list.length;
  const ncols = Math.max(1, Math.min(6, Math.round(Math.sqrt(n * 1.15))));
  const colH = new Array<number>(ncols).fill(0);
  c.pos = [];
  for (const t of c.list) {
    const card = cards[c.name + '.' + t.name];
    const ci = shortestColumn(colH);
    c.pos.push({ key: c.name + '.' + t.name, dx: ci * (CARD_W + GAP_X), dy: colH[ci] });
    colH[ci] += card.h + GAP_Y;
  }
  c.w = ncols * CARD_W + (ncols - 1) * GAP_X + CL_PAD * 2;
  c.h = Math.max(...colH) - GAP_Y + CL_PAD * 2 + CL_TITLE;
}

const byArea = (a: Cluster, b: Cluster) => (b.w ?? 0) * (b.h ?? 0) - (a.w ?? 0) * (a.h ?? 0);

/** Count inter-schema ref links (symmetric), keyed 'schemaA|schemaB'. */
function countSchemaLinks(data: LayoutData): Record<string, number> {
  const links: Record<string, number> = {};
  for (const r of data.refs) {
    if (r.from.s === r.to.s) continue;
    links[r.from.s + '|' + r.to.s] = (links[r.from.s + '|' + r.to.s] || 0) + 1;
    links[r.to.s + '|' + r.from.s] = (links[r.to.s + '|' + r.from.s] || 0) + 1;
  }
  return links;
}

/** Remove + return the `left` cluster most strongly linked to the already-`ordered` ones. */
function pickNextLinked(left: Cluster[], ordered: Cluster[], links: Record<string, number>): Cluster {
  const lk = (a: Cluster, b: Cluster) => links[a.name + '|' + b.name] || 0;
  let best = 0, bestScore = -1;
  for (let i = 0; i < left.length; i++) {
    let score = 0;
    for (let j = 0; j < ordered.length; j++) score += lk(left[i], ordered[j]) * (j + 1);
    if (score > bestScore) { bestScore = score; best = i; }
  }
  return left.splice(best, 1)[0];
}

/**
 * Order clusters. 'untangle' greedily chains schemas so heavily-linked ones stay
 * adjacent (seeded by the largest cluster); otherwise sort by area, biggest first.
 */
export function orderClusters(clusters: Cluster[], data: LayoutData, arrange: Arrange): Cluster[] {
  if (arrange !== 'untangle' || clusters.length <= 1) {
    return clusters.sort(byArea);
  }
  const links = countSchemaLinks(data);
  const left = clusters.slice().sort(byArea);
  const ordered: Cluster[] = [left.shift()!];
  while (left.length) ordered.push(pickNextLinked(left, ordered, links));
  return ordered;
}

type FlowCursor = { x: number; y: number; rowH: number };

/** Place one cluster's origin (wrapping the row first if it would overflow), then its cards. */
function placeCluster(c: Cluster, cursor: FlowCursor, cards: Cards): void {
  if (cursor.x > 0 && cursor.x + (c.w ?? 0) > MAX_ROW_W) {
    cursor.x = 0; cursor.y += cursor.rowH + CL_GAP_Y; cursor.rowH = 0;
  }
  c.x = cursor.x; c.y = cursor.y;
  cursor.x += (c.w ?? 0) + CL_GAP_X;
  cursor.rowH = Math.max(cursor.rowH, c.h ?? 0);
  for (const p of (c.pos ?? [])) {
    const card = cards[p.key];
    card.x = c.x + CL_PAD + p.dx;
    card.y = c.y + CL_PAD + CL_TITLE + p.dy;
    card.hue = c.hue;
  }
}

/** Flow clusters into rows (wrapping at MAX_ROW_W), place each card, return canvas size. */
export function flow(clusters: Cluster[], cards: Cards): Size {
  const cursor: FlowCursor = { x: 0, y: 0, rowH: 0 };
  for (const c of clusters) placeCluster(c, cursor, cards);
  return {
    w: Math.max(...clusters.map((c) => c.x + (c.w ?? 0))) + 60,
    h: cursor.y + cursor.rowH + 60,
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

/** Reorder one cluster's tables toward their neighbors' barycenters, then re-pack it. */
function reorderTowardNeighbors(c: Cluster, cards: Cards, nbrs: Record<string, string[]>): void {
  const score: Record<string, number> = {};
  for (const t of c.list) score[c.name + '.' + t.name] = barycenter(c.name + '.' + t.name, nbrs, cards);
  c.list = c.list.slice().sort((a, b) => score[c.name + '.' + a.name] - score[c.name + '.' + b.name]);
  pack(c, cards);
}

/** Two barycenter passes: reorder each cluster's tables toward neighbors, re-pack, re-flow. */
export function barycenterPasses(clusters: Cluster[], cards: Cards, nbrs: Record<string, string[]>): Size {
  let size: Size = { w: 0, h: 0 };
  for (let iter = 0; iter < 2; iter++) {
    for (const c of clusters) reorderTowardNeighbors(c, cards, nbrs);
    size = flow(clusters, cards);
  }
  return size;
}
