import { it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Diagram from './Diagram.svelte';
import { createViewerState } from './state.svelte';
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
      columns: [
        { name: 'id', type: 'uuid', pk: true },
        { name: 'lookup_id', type: 'uuid' },
      ],
    },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('renders cards and an edge, and clicking a card enters focus', async () => {
  const state = createViewerState();
  const { container } = render(Diagram, { props: { model: MODEL, state } });
  expect(container.querySelectorAll('[data-card]').length).toBe(2);
  expect(container.querySelectorAll('path').length).toBeGreaterThanOrEqual(1);
  const card = container.querySelector('[data-card="config.lookup_values"]') as HTMLElement;
  await fireEvent.click(card);
  expect(state.selected).toBe('config.lookup_values');
  expect(state.mode).toBe('focus');
});
