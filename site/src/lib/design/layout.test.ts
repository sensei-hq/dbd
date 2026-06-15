import { it, expect } from 'vitest';
import { compute, edgePath } from './layout';
import { toLayoutData, type SchemaModel } from './model';

const MODEL: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table',
      columns: [{ name: 'id', type: 'uuid', pk: true }, { name: 'lookup_id', type: 'uuid' }] },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('compute returns cards positioned and one edge', () => {
  const data = toLayoutData(MODEL);
  const a = compute(data, 'keys', 'a-z');
  expect(Object.keys(a.cards)).toContain('config.lookups');
  expect(a.cards['config.lookups'].w).toBe(248);
  expect(a.edges).toHaveLength(1);
  // determinism: same input → same geometry
  const b = compute(data, 'keys', 'a-z');
  expect(b.cards['config.lookups'].x).toBe(a.cards['config.lookups'].x);
  expect(typeof edgePath(a.edges[0], 'curved')).toBe('string');
});
