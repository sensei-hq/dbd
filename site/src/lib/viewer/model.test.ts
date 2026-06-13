import { it, expect } from 'vitest';
import { toLayoutData, neighborsOf, type SchemaModel } from './model';

const model: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table',
      columns: [{ name: 'id', type: 'uuid', pk: true }, { name: 'lookup_id', type: 'uuid' }] },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('derives per-column fk flags from refs', () => {
  const data = toLayoutData(model);
  const lv = data.tables.find((t) => t.name === 'lookup_values')!;
  expect(lv.columns.find((c) => c.name === 'lookup_id')!.fk).toBe(true);
  expect(lv.columns.find((c) => c.name === 'id')!.fk).toBeFalsy();
  expect(data.refs).toHaveLength(1);
});

it('neighborsOf returns from+to connected tables', () => {
  // forward (FK origin → target) and reverse (target → FK origin)
  expect(neighborsOf(model, 'config.lookup_values').has('config.lookups')).toBe(true);
  expect(neighborsOf(model, 'config.lookups').has('config.lookup_values')).toBe(true);
});
