import { it, expect } from 'vitest';
import { encodeFragment, decodeFragment } from './fragment';

const model = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 1, enums: 0 }],
  tables: [{ schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] }],
  refs: [],
};

it('round-trips a model through encode/decode', async () => {
  const frag = await encodeFragment(model);
  expect(frag.startsWith('1.')).toBe(true);
  const back = await decodeFragment('#' + frag);
  expect(back).toEqual(model);
});

it('rejects malformed or unknown-version fragments', async () => {
  await expect(decodeFragment('#nope')).rejects.toThrow();
  await expect(decodeFragment('#9.AAAA')).rejects.toThrow();
});

it('rejects a valid-version fragment whose payload is not gzip', async () => {
  // version ok, base64 decodes, but the bytes aren't gzip → gunzip must throw
  await expect(decodeFragment('#1.AAAA')).rejects.toThrow();
});
