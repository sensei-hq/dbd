import { it, expect, beforeEach } from 'vitest';
import { saveDiagram, listDiagrams, deleteDiagram, keyFor } from './store';
import type { SchemaModel } from '$lib/design/model';

const model = (name: string): SchemaModel => ({
  project: { name, db: 'postgresql' },
  schemas: [{ name: 's', tables: 1, enums: 0 }],
  tables: [{ schema: 's', name: 't', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true, nn: true }] }],
  refs: [],
});

beforeEach(() => localStorage.clear());

it('saves under dbd:diagram:<project> with a 1.<payload> value and lists it', async () => {
  await saveDiagram(model('alpha'));
  expect(localStorage.getItem(keyFor('alpha'))).toMatch(/^1\./);
  const list = await listDiagrams();
  expect(list.map((d) => d.project)).toEqual(['alpha']);
  expect(list[0].model.project.name).toBe('alpha'); // round-trips through gzip+base64url
});

it('lists multiple sorted by name and deletes one', async () => {
  await saveDiagram(model('beta'));
  await saveDiagram(model('alpha'));
  expect((await listDiagrams()).map((d) => d.project)).toEqual(['alpha', 'beta']);
  deleteDiagram('alpha');
  expect((await listDiagrams()).map((d) => d.project)).toEqual(['beta']);
});

it('ignores unrelated and corrupt keys', async () => {
  localStorage.setItem('unrelated', 'x');
  localStorage.setItem(keyFor('broken'), 'not-a-valid-payload');
  await saveDiagram(model('good'));
  expect((await listDiagrams()).map((d) => d.project)).toEqual(['good']);
});
