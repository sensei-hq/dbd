/* Characterization tests for ER diagram edge routing (layout-edges.ts).
   These pin the CURRENT geometry so a later complexity-reducing refactor
   (extracting orthogonalPath/curvedPath out of edgePath) can be verified
   as behavior-preserving. */

import { it, expect, describe } from 'vitest';
import { buildEdges, edgePath } from './layout-edges';
import type { Cards, Card, Edge, LayoutData, LayoutColumn } from './layout-types';

function col(name: string): LayoutColumn {
  return { name, type: 'text', fk: false };
}

function makeCard(x: number, y: number, w: number, h: number, vis: LayoutColumn[] = []): Card {
  return { t: { schema: 's', name: 't', columns: [] }, vis, more: 0, w, h, x, y };
}

describe('buildEdges / buildEdge', () => {
  it('skips a ref whose endpoint is not laid out', () => {
    const cards: Cards = { 'a.x': makeCard(0, 0, 248, 100) };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } }],
    };
    expect(buildEdges(data, cards)).toEqual([]);
  });

  it('builds a self-loop edge on the right edge (y1 !== y2)', () => {
    const cards: Cards = { 'a.x': makeCard(0, 0, 248, 100, [col('id'), col('parent_id')]) };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'parent_id' }, to: { s: 'a', t: 'x', c: 'id' } }],
    };
    const edges = buildEdges(data, cards);
    expect(edges).toHaveLength(1);
    expect(edges[0]).toMatchObject({
      i: 0, fromKey: 'a.x', toKey: 'a.x', self: true,
      x1: 248, y1: 76, x2: 248, y2: 52, s1: 1, s2: 1,
    });
  });

  it('nudges a self-loop endpoint by +14 when both anchors land on the same y', () => {
    const cards: Cards = { 'a.x': makeCard(0, 0, 248, 100, [col('id'), col('parent_id')]) };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'a', t: 'x', c: 'id' } }],
    };
    const edges = buildEdges(data, cards);
    expect(edges[0]).toMatchObject({ self: true, x1: 248, y1: 52, x2: 248, y2: 66, s1: 1, s2: 1 });
  });

  it('routes right-to-left when a sits far enough left of b', () => {
    const cards: Cards = {
      'a.x': makeCard(0, 0, 248, 100, [col('id')]),
      'b.y': makeCard(300, 0, 248, 100, [col('id')]),
    };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } }],
    };
    const edges = buildEdges(data, cards);
    expect(edges[0]).toMatchObject({ self: false, x1: 248, y1: 52, x2: 300, y2: 52, s1: 1, s2: -1 });
  });

  it('routes left-to-right when b sits far enough left of a', () => {
    const cards: Cards = {
      'a.x': makeCard(400, 0, 100, 100, [col('id')]),
      'b.y': makeCard(0, 0, 200, 100, [col('id')]),
    };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } }],
    };
    const edges = buildEdges(data, cards);
    expect(edges[0]).toMatchObject({ self: false, x1: 400, y1: 52, x2: 200, y2: 52, s1: -1, s2: 1 });
  });

  it('falls back to the stacked (both-right) route when neither side has a 50px gap', () => {
    const cards: Cards = {
      'a.x': makeCard(0, 0, 248, 100, [col('id')]),
      'b.y': makeCard(100, 0, 248, 100, [col('id')]),
    };
    const data: LayoutData = {
      tables: [],
      refs: [{ from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } }],
    };
    const edges = buildEdges(data, cards);
    expect(edges[0]).toMatchObject({ self: false, x1: 248, y1: 52, x2: 348, y2: 52, s1: 1, s2: 1 });
  });

  it('preserves the original refs index even when an earlier ref is skipped', () => {
    const cards: Cards = {
      'a.x': makeCard(0, 0, 248, 100, [col('id')]),
      'b.y': makeCard(300, 0, 248, 100, [col('id')]),
    };
    const data: LayoutData = {
      tables: [],
      refs: [
        { from: { s: 'zz', t: 'missing', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } },
        { from: { s: 'a', t: 'x', c: 'id' }, to: { s: 'b', t: 'y', c: 'id' } },
      ],
    };
    const edges = buildEdges(data, cards);
    expect(edges).toHaveLength(1);
    expect(edges[0].i).toBe(1);
  });
});

describe('edgePath', () => {
  const base = { i: 0, ref: {} as Edge['ref'], fromKey: 'a', toKey: 'b' };

  it('draws a self-loop bow regardless of style', () => {
    const e: Edge = { ...base, self: true, x1: 10, y1: 20, x2: 10, y2: 60, s1: 1, s2: 1 };
    expect(edgePath(e, 'curved')).toBe('M 10 20 C 56 20, 56 60, 10 60');
    expect(edgePath(e, 'orthogonal')).toBe('M 10 20 C 56 20, 56 60, 10 60');
  });

  it('draws an orthogonal path via the midpoint when sides are opposite (s1=1,s2=-1)', () => {
    const e: Edge = { ...base, self: false, x1: 0, y1: 10, x2: 300, y2: 200, s1: 1, s2: -1 };
    expect(edgePath(e, 'orthogonal')).toBe('M 0 10 H 150 V 200 H 300');
  });

  it('draws an orthogonal path via the midpoint when sides are opposite (s1=-1,s2=1)', () => {
    const e: Edge = { ...base, self: false, x1: 300, y1: 10, x2: 0, y2: 200, s1: -1, s2: 1 };
    expect(edgePath(e, 'orthogonal')).toBe('M 300 10 H 150 V 200 H 0');
  });

  it('draws an orthogonal stacked-fallback path routed around the right (s1=1,s2=1)', () => {
    const e: Edge = { ...base, self: false, x1: 100, y1: 10, x2: 150, y2: 200, s1: 1, s2: 1 };
    expect(edgePath(e, 'orthogonal')).toBe('M 100 10 H 202 V 200 H 150');
  });

  it('draws a curved path bowing the same direction when s1 === s2', () => {
    const e: Edge = { ...base, self: false, x1: 100, y1: 10, x2: 150, y2: 200, s1: 1, s2: 1 };
    expect(edgePath(e, 'curved')).toBe('M 100 10 C 164 10, 214 200, 150 200');
  });

  it('draws a curved path with the control-point offset clamped to a 46px minimum', () => {
    const e: Edge = { ...base, self: false, x1: 100, y1: 10, x2: 110, y2: 200, s1: 1, s2: -1 };
    expect(edgePath(e, 'curved')).toBe('M 100 10 C 146 10, 64 200, 110 200');
  });

  it('draws a curved path with the control-point offset clamped to a 170px maximum', () => {
    const e: Edge = { ...base, self: false, x1: 0, y1: 10, x2: 1000, y2: 200, s1: 1, s2: -1 };
    expect(edgePath(e, 'curved')).toBe('M 0 10 C 170 10, 830 200, 1000 200');
  });

  it('draws a curved path with an unclamped control-point offset (half the x distance)', () => {
    const e: Edge = { ...base, self: false, x1: 0, y1: 10, x2: 200, y2: 200, s1: 1, s2: -1 };
    expect(edgePath(e, 'curved')).toBe('M 0 10 C 100 10, 100 200, 200 200');
  });
});
