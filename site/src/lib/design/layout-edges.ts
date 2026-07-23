/* ER diagram layout: edge routing — anchor points, connector geometry, SVG paths. */

import type { LayoutData, Ref, Card, Cards, Edge, EdgeStyle } from './layout-types';
import { HEAD_H, ROW_H } from './layout-types';

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
export function buildEdges(data: LayoutData, cards: Cards): Edge[] {
  const edges: Edge[] = [];
  data.refs.forEach((r, i) => {
    const e = buildEdge(r, i, cards);
    if (e) edges.push(e);
  });
  return edges;
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
