import { it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import EntitiesList from './EntitiesList.svelte';
import type { SchemaModel } from './model';

const MODEL: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', noteMd: 'A **lookup** table.', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    {
      schema: 'config',
      name: 'lookup_values',
      kind: 'table',
      columns: [
        { name: 'id', type: 'uuid', pk: true },
        { name: 'lookup_id', type: 'uuid' },
      ],
    },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('renders one row per table', () => {
  const { container } = render(EntitiesList, { props: { model: MODEL, onNav: () => {} } });
  const rows = [...container.querySelectorAll('[data-entity-row]')];
  expect(rows.map((r) => r.getAttribute('data-entity-row'))).toEqual([
    'config.lookups',
    'config.lookup_values',
  ]);
});

it('calls onNav with the entity key when a row is clicked', async () => {
  const onNav = vi.fn();
  const { container } = render(EntitiesList, { props: { model: MODEL, onNav } });
  const row = container.querySelector('[data-entity-row="config.lookup_values"]') as HTMLElement;
  await fireEvent.click(row);
  expect(onNav).toHaveBeenCalledWith('config.lookup_values');
});
