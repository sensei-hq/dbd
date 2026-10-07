import { it, expect } from 'vitest';
import { validateModel, nodeId, withStubs, type SchemaModel } from './model';

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

it('refuses a model newer than this viewer reads, instead of rendering it wrong', () => {
  // A newer dbd can emit a model this viewer predates. Loading it would drop whatever the new
  // version added without a word; saying so is the only honest outcome.
  const newer = validateModel({ ...v2, version: 4 });
  expect(newer.ok).toBe(false);
  if (!newer.ok) expect(newer.error).toContain('version 4');
  expect(validateModel({ ...v2, version: 3 }).ok).toBe(true);
  expect(validateModel({ ...v2, version: 'three' }).ok).toBe(false);
});

it('accepts the stubs a v3 model carries, and refuses a malformed one at the boundary', () => {
  // The tables a model's foreign keys land on but does not carry — fed to the graph as nodes,
  // so a malformed entry would fail inside the renderer instead of here.
  const stub = { schema: 'auth', name: 'users', kind: 'external', columns: [{ name: 'id', type: 'uuid' }] };
  expect(validateModel({ ...v1, version: 3, stubs: [stub] }).ok).toBe(true);
  expect(validateModel({ ...v1, stubs: {} }).ok).toBe(false);
  expect(validateModel({ ...v1, stubs: [{ name: 'users' }] }).ok).toBe(false);
});

it('hands the ER graph the stubs beside the tables, and leaves the tables as they are', () => {
  const stub = { schema: 'auth', name: 'users', kind: 'external', columns: [{ name: 'id', type: 'uuid' }] };
  const m: SchemaModel = { ...v1, stubs: [stub] };
  expect(withStubs(m).tables).toEqual([...v1.tables, stub]);
  expect(m.tables).toEqual(v1.tables);
  expect(withStubs(v1).tables).toEqual(v1.tables);
});
