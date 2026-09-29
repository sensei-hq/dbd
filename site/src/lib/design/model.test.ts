import { it, expect } from 'vitest';
import { validateModel, nodeId, type SchemaModel } from './model';

/* `toLayoutData` and `neighborsOf` are gone with the local layout: the package derives the
   fk flag and the neighbour set itself, and v2 declares `fk` on the column outright. What is
   left to pin here is the CONTRACT — that v1 payloads still load, and that a v2 one keeps
   both halves. */

const v1: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table',
      columns: [{ name: 'id', type: 'uuid', pk: true }, { name: 'lookup_id', type: 'uuid' }] },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

const v2: SchemaModel = {
  ...v1,
  version: 2,
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table',
      columns: [{ name: 'id', type: 'uuid', pk: true, uq: true }, { name: 'owner', type: 'uuid', fk: true }] },
  ],
  entities: [
    { schema: 'config', name: 'active_lookups', kind: 'view', noteMd: 'Undeleted rows.' },
    { schema: 'config', name: 'purge', kind: 'procedure' },
  ],
  deps: [
    { from: { s: 'config', n: 'active_lookups' }, to: { s: 'config', n: 'lookups' }, kind: 'reads' },
    { from: { s: 'config', n: 'purge' }, to: { s: '', n: 'now' }, kind: 'calls', unresolved: true },
  ],
};

it('accepts a well-formed model and rejects malformed ones', () => {
  const good = { project: { name: 'p', db: 'pg' }, schemas: [], tables: [], refs: [] };
  expect(validateModel(good).ok).toBe(true);
  expect(validateModel(null).ok).toBe(false);
  expect(validateModel({ project: {} }).ok).toBe(false);
  expect(validateModel({ project: { name: 'p' }, schemas: [], tables: [{ name: 'x' }], refs: [] }).ok).toBe(false);
});

it('still accepts a v1 payload, which every share link encoded before the upgrade is', () => {
  // The whole reason `version`, `entities` and `deps` are optional.
  const res = validateModel(v1);
  expect(res.ok).toBe(true);
  if (res.ok) expect(res.model.version).toBeUndefined();
});

it('keeps both halves of a v2 payload', () => {
  const res = validateModel(v2);
  expect(res.ok).toBe(true);
  if (!res.ok) return;
  // tables/refs is the ER graph; entities/deps is the dependency graph.
  expect(res.model.version).toBe(2);
  expect(res.model.entities?.map((e) => e.kind)).toEqual(['view', 'procedure']);
  expect(res.model.deps).toHaveLength(2);
});

it('carries the v2 column flags dbd now declares', () => {
  // v1 had neither: `fk` was derived from refs and `uq` was only reachable via Index.unique.
  const res = validateModel(v2);
  if (!res.ok) throw new Error('expected valid');
  const cols = res.model.tables[0].columns;
  expect(cols.find((c) => c.name === 'owner')?.fk).toBe(true);
  expect(cols.find((c) => c.name === 'id')?.uq).toBe(true);
});

it('marks an unresolvable dependency rather than omitting it', () => {
  // A call to a built-in. The edge is real; only its endpoint is unplaceable.
  const res = validateModel(v2);
  if (!res.ok) throw new Error('expected valid');
  expect(res.model.deps?.find((d) => d.to.n === 'now')?.unresolved).toBe(true);
});

it('rejects a v2 half that is present but the wrong shape', () => {
  // Otherwise it reaches the renderer and fails there instead of at the boundary.
  expect(validateModel({ ...v1, entities: {} }).ok).toBe(false);
  expect(validateModel({ ...v1, deps: 'nope' }).ok).toBe(false);
});

it('builds a node id from schema and name', () => {
  expect(nodeId('config', 'lookups')).toBe('config.lookups');
});
