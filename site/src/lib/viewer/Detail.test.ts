import { it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import Detail from './Detail.svelte';
import type { SchemaModel } from './model';

const MODEL: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    {
      schema: 'config',
      name: 'lookup_values',
      kind: 'table',
      noteMd: 'Stores **lookup** values.',
      columns: [
        { name: 'id', type: 'uuid', pk: true },
        { name: 'lookup_id', type: 'uuid', nn: true },
      ],
    },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('shows columns with PK/FK badges and renders the note markdown', () => {
  const { container } = render(Detail, { props: { model: MODEL, selected: 'config.lookup_values' } });
  // note markdown rendered (bold)
  expect(container.querySelector('strong')?.textContent).toBe('lookup');
  // id row → PK badge; lookup_id row → FK badge
  const text = container.textContent ?? '';
  expect(text).toContain('id');
  expect(text).toContain('lookup_id');
  // find the row containing 'lookup_id' and assert it has FK + NN badges
  const rows = [...container.querySelectorAll('[data-col-row]')];
  const fkRow = rows.find((r) => r.getAttribute('data-col-row') === 'lookup_id')!;
  expect(fkRow.textContent).toContain('FK');
  expect(fkRow.textContent).toContain('NN');
  const pkRow = rows.find((r) => r.getAttribute('data-col-row') === 'id')!;
  expect(pkRow.textContent).toContain('PK');
});

it('renders nothing meaningful when nothing is selected', () => {
  const { container } = render(Detail, { props: { model: MODEL, selected: null } });
  expect(container.querySelectorAll('[data-col-row]').length).toBe(0);
});
