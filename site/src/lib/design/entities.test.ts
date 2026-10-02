import { it, expect, describe } from 'vitest';
import { allEntities, entityAt, kindCounts, KIND_ORDER } from './entities';
import { sampleModel } from './data';

// Every entity in one list (#34): tables, the v2 entities and the v3 enums, keyed `schema.name`
// — the id the diagram gives the same node — and re-derived here from the model's own arrays.

describe('allEntities', () => {
  it('lists every table, view, routine and enum the model carries', () => {
    const keys = (kind: string) => allEntities(sampleModel).filter((e) => e.kind === kind).map((e) => e.key);
    expect(keys('table')).toEqual(sampleModel.tables.map((t) => `${t.schema}.${t.name}`).sort());
    expect(keys('view')).toEqual(['shop.order_totals']);
    expect(keys('procedure')).toEqual(['shop.place_order']);
    expect(keys('enum')).toEqual((sampleModel.enums ?? []).map((e) => `${e.schema}.${e.name}`));
  });

  it('orders by schema, then kind, then name', () => {
    const shop = allEntities(sampleModel).filter((e) => e.schema === 'shop');
    const rank = (k: string) => KIND_ORDER.indexOf(k as never);
    for (let i = 1; i < shop.length; i++) {
      const [a, b] = [shop[i - 1], shop[i]];
      expect(rank(a.kind) < rank(b.kind) || (a.kind === b.kind && a.name < b.name)).toBe(true);
    }
  });

  it('reads a v1 model as tables alone', () => {
    const { entities: _e, deps: _d, enums: _n, history: _h, version: _v, ...v1 } = sampleModel;
    expect(new Set(allEntities(v1).map((e) => e.kind))).toEqual(new Set(['table']));
  });
});

describe('entityAt', () => {
  it('resolves a key to its entity, whatever its kind', () => {
    expect(entityAt(sampleModel, 'shop.orders')?.kind).toBe('table');
    expect(entityAt(sampleModel, 'shop.order_totals')?.kind).toBe('view');
    expect(entityAt(sampleModel, 'shop.order_status')?.kind).toBe('enum');
    expect(entityAt(sampleModel, 'shop.nope')).toBeUndefined();
  });
});

describe('kindCounts', () => {
  it('counts each kind present, in kind order, and leaves out the absent ones', () => {
    expect(kindCounts(sampleModel)).toEqual([
      { kind: 'table', count: 6 },
      { kind: 'view', count: 1 },
      { kind: 'procedure', count: 1 },
      { kind: 'enum', count: 1 },
    ]);
  });
});
