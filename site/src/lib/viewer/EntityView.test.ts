import { it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import EntityView from './EntityView.svelte';
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
        // FK column: a ref leaves from it (derives the FK badge).
        { name: 'lookup_id', type: 'uuid', nn: true },
        { name: 'label', type: 'varchar(64)' },
      ],
    },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('renders the columns table with PK/FK badges and the rendered comment markdown', () => {
  const { container } = render(EntityView, {
    props: { model: MODEL, entityKey: 'config.lookup_values', onNav: () => {} },
  });

  // comment markdown rendered (bold from **lookup**)
  expect(container.querySelector('strong')?.textContent).toBe('lookup');

  // column names show up
  const rows = [...container.querySelectorAll('[data-col-row]')];
  expect(rows.map((r) => r.getAttribute('data-col-row'))).toEqual(['id', 'lookup_id', 'label']);

  // PK badge on id, FK badge on lookup_id (derived from refs)
  const pkRow = rows.find((r) => r.getAttribute('data-col-row') === 'id')!;
  expect(pkRow.querySelector('.col-badge.pk')?.textContent).toBe('PK');
  const fkRow = rows.find((r) => r.getAttribute('data-col-row') === 'lookup_id')!;
  expect(fkRow.querySelector('.col-badge.fk')?.textContent).toBe('FK');
  expect(fkRow.textContent).toContain('NN');
});

it('navigates via a column ref button', async () => {
  const onNav = vi.fn();
  const { container } = render(EntityView, {
    props: { model: MODEL, entityKey: 'config.lookup_values', onNav },
  });
  const fkRow = [...container.querySelectorAll('[data-col-row]')].find(
    (r) => r.getAttribute('data-col-row') === 'lookup_id'
  )!;
  const btn = fkRow.querySelector('button')!;
  await fireEvent.click(btn);
  expect(onNav).toHaveBeenCalledWith('config.lookups');
});
