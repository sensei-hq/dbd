/* Characterization tests for ER diagram cluster building/ordering/packing/flow
   (layout-clusters.ts). These pin the CURRENT algorithm output so a later
   complexity-reducing refactor of the packing/flow loop can be verified as
   behavior-preserving. */

import { it, expect, describe } from 'vitest';
import {
  groupBySchema, buildAdjacency, buildClusters, pack, orderClusters, flow, barycenterPasses,
} from './layout-clusters';
import { HUES } from './layout-types';
import type { Cards, Card, Cluster, LayoutData, LayoutTable } from './layout-types';

function makeCard(x: number, y: number, w: number, h: number): Card {
  return { t: { schema: 's', name: 't', columns: [] }, vis: [], more: 0, w, h, x, y };
}

function table(schema: string, name: string): LayoutTable {
  return { schema, name, columns: [] };
}

describe('groupBySchema', () => {
  it('groups tables by schema, preserving each schema\'s original table order', () => {
    const data: LayoutData = {
      tables: [table('b', 't1'), table('a', 't2'), table('b', 't3')],
      refs: [],
    };
    const grouped = groupBySchema(data);
    expect(Object.keys(grouped).sort()).toEqual(['a', 'b']);
    expect(grouped.b.map((t) => t.name)).toEqual(['t1', 't3']);
    expect(grouped.a.map((t) => t.name)).toEqual(['t2']);
  });
});

describe('buildAdjacency', () => {
  it('builds a bidirectional neighbor map and skips same-table (self) refs', () => {
    const data: LayoutData = {
      tables: [],
      refs: [
        { from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'a', t: 'x', c: 'id' } },
        { from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } },
      ],
    };
    const nbrs = buildAdjacency(data);
    expect(nbrs['a.x']).toEqual(['b.y']);
    expect(nbrs['b.y']).toEqual(['a.x']);
  });
});

describe('buildClusters', () => {
  it('sorts schemas alphabetically, sorts each list, and assigns hue by a-z position', () => {
    const bySchema: Record<string, LayoutTable[]> = {
      zebra: [table('zebra', 'z1')],
      apple: [table('apple', 'a2'), table('apple', 'a1')],
    };
    const clusters = buildClusters(bySchema);
    expect(clusters.map((c) => c.name)).toEqual(['apple', 'zebra']);
    expect(clusters[0].list.map((t) => t.name)).toEqual(['a1', 'a2']);
    expect(clusters[0].count).toBe(2);
    expect(clusters[0].hue).toBe(HUES[0]);
    expect(clusters[1].hue).toBe(HUES[1]);
  });
});

describe('pack', () => {
  it('masonry-packs a 3-table cluster into the shortest column each step', () => {
    const cluster: Cluster = {
      name: 's', list: [table('s', 't1'), table('s', 't2'), table('s', 't3')],
      count: 3, hue: 0, x: 0, y: 0,
    };
    const cards: Cards = {
      's.t1': makeCard(0, 0, 248, 100),
      's.t2': makeCard(0, 0, 248, 150),
      's.t3': makeCard(0, 0, 248, 80),
    };
    pack(cluster, cards);
    expect(cluster.pos).toEqual([
      { key: 's.t1', dx: 0, dy: 0 },
      { key: 's.t2', dx: 284, dy: 0 },
      { key: 's.t3', dx: 0, dy: 130 },
    ]);
    expect(cluster.w).toBe(584);
    expect(cluster.h).toBe(278);
  });

  it('packs a single-table cluster into one column', () => {
    const cluster: Cluster = { name: 's', list: [table('s', 't1')], count: 1, hue: 0, x: 0, y: 0 };
    const cards: Cards = { 's.t1': makeCard(0, 0, 248, 100) };
    pack(cluster, cards);
    expect(cluster.pos).toEqual([{ key: 's.t1', dx: 0, dy: 0 }]);
    expect(cluster.w).toBe(300);
    expect(cluster.h).toBe(168);
  });
});

describe('orderClusters', () => {
  const A: Cluster = { name: 'A', list: [], count: 0, hue: 0, x: 0, y: 0, w: 5, h: 2 };
  const B: Cluster = { name: 'B', list: [], count: 0, hue: 0, x: 0, y: 0, w: 10, h: 10 };
  const C: Cluster = { name: 'C', list: [], count: 0, hue: 0, x: 0, y: 0, w: 10, h: 5 };

  it('sorts by area (biggest first) for any arrange other than untangle', () => {
    const data: LayoutData = { tables: [], refs: [] };
    const ordered = orderClusters([A, B, C], data, 'a-z');
    expect(ordered.map((c) => c.name)).toEqual(['B', 'C', 'A']);
  });

  it('sorts by area for untangle when there is only one cluster (no chaining possible)', () => {
    const data: LayoutData = { tables: [], refs: [] };
    const ordered = orderClusters([A], data, 'untangle');
    expect(ordered.map((c) => c.name)).toEqual(['A']);
  });

  it('greedily chains schemas by inter-schema link weight for untangle', () => {
    const refs: LayoutData['refs'] = [];
    const push = (s1: string, s2: string, n: number) => {
      for (let i = 0; i < n; i++) {
        refs.push({ from: { s: s1, t: `t${i}${s1}${s2}`, c: 'id' }, to: { s: s2, t: `u${i}${s1}${s2}`, c: 'id' } });
      }
    };
    push('A', 'B', 5); // strong A-B link
    push('C', 'B', 1); // weak C-B link
    push('C', 'A', 2); // medium C-A link
    const data: LayoutData = { tables: [], refs };
    // Seed = largest by area (B). Then greedily picks the best-linked remaining
    // cluster at each step (A beats C against {B}; C is then appended).
    const ordered = orderClusters([A, B, C], data, 'untangle');
    expect(ordered.map((c) => c.name)).toEqual(['B', 'A', 'C']);
  });
});

describe('flow', () => {
  it('lays out clusters left-to-right in a row and places their cards (no wrap)', () => {
    const c1: Cluster = {
      name: 'a', list: [], count: 0, hue: 0, x: 0, y: 0, w: 500, h: 300,
      pos: [{ key: 'a.t1', dx: 0, dy: 0 }],
    };
    const c2: Cluster = {
      name: 'b', list: [], count: 0, hue: 0, x: 0, y: 0, w: 400, h: 200,
      pos: [{ key: 'b.t1', dx: 10, dy: 20 }],
    };
    const cards: Cards = { 'a.t1': makeCard(0, 0, 248, 100), 'b.t1': makeCard(0, 0, 248, 100) };
    const size = flow([c1, c2], cards);

    expect(c1.x).toBe(0); expect(c1.y).toBe(0);
    expect(c2.x).toBe(610); expect(c2.y).toBe(0);
    expect(cards['a.t1']).toMatchObject({ x: 26, y: 42, hue: 0 });
    expect(cards['b.t1']).toMatchObject({ x: 646, y: 62, hue: 0 });
    expect(size).toEqual({ w: 1070, h: 360 });
  });

  it('wraps to a new row when a cluster would exceed MAX_ROW_W', () => {
    const c1: Cluster = { name: 'a', list: [], count: 0, hue: 0, x: 0, y: 0, w: 2000, h: 100, pos: [] };
    const c2: Cluster = { name: 'b', list: [], count: 0, hue: 0, x: 0, y: 0, w: 2000, h: 50, pos: [] };
    const size = flow([c1, c2], {});

    expect(c1.x).toBe(0); expect(c1.y).toBe(0);
    expect(c2.x).toBe(0); expect(c2.y).toBe(210); // wrapped below row 1 (100 + CL_GAP_Y 110)
    expect(size).toEqual({ w: 2060, h: 320 });
  });
});

describe('barycenterPasses', () => {
  it('reorders a cluster\'s tables toward their neighbors\' positions over two passes', () => {
    const bySchema: Record<string, LayoutTable[]> = {
      s: [table('s', 't1'), table('s', 't2'), table('s', 't3')],
    };
    const clusters = buildClusters(bySchema);
    const cards: Cards = {
      's.t1': makeCard(0, 0, 248, 60),
      's.t2': makeCard(0, 0, 248, 120),
      's.t3': makeCard(0, 0, 248, 90),
      'anchor.hi': makeCard(0, 500, 248, 40),
      'anchor.lo': makeCard(0, 10, 248, 40),
    };
    // t1 is pulled toward a low-positioned neighbor (high y), t3 toward a
    // high-positioned neighbor (low y); t2 has no neighbors (self-anchored).
    const nbrs: Record<string, string[]> = { 's.t1': ['anchor.hi'], 's.t3': ['anchor.lo'] };

    clusters.forEach((c) => pack(c, cards));
    const size = barycenterPasses(clusters, cards, nbrs);

    expect(clusters[0].list.map((t) => t.name)).toEqual(['t3', 't2', 't1']);
    expect(clusters[0].pos).toEqual([
      { key: 's.t3', dx: 0, dy: 0 },
      { key: 's.t2', dx: 284, dy: 0 },
      { key: 's.t1', dx: 0, dy: 120 },
    ]);
    expect(cards['s.t3']).toMatchObject({ x: 26, y: 42 });
    expect(cards['s.t2']).toMatchObject({ x: 310, y: 42 });
    expect(cards['s.t1']).toMatchObject({ x: 26, y: 162 });
    expect(size).toEqual({ w: 644, h: 308 });
  });
});
