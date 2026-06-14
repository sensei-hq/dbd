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

it('focus mode shows only the selected card plus its neighbors and re-fits without crashing', async () => {
  const state = createViewerState();
  const { container } = render(Diagram, { props: { model: MODEL, state } });
  const card = container.querySelector('[data-card="config.lookup_values"]') as HTMLElement;
  // Clicking enters focus and triggers the focus-fit $effect (which runs fit()
  // against jsdom's zero-size viewport — it must not throw / loop).
  await fireEvent.click(card);
  expect(state.mode).toBe('focus');
  // lookup_values + its single neighbor lookups = 2 visible cards.
  expect(container.querySelectorAll('[data-card]').length).toBe(2);
});

it('calls onSelect (and does not mutate state) when the callback prop is provided', async () => {
  const state = createViewerState();
  const calls: (string | null)[] = [];
  const { container } = render(Diagram, {
    props: { model: MODEL, state, onSelect: (k: string | null) => calls.push(k) },
  });
  const card = container.querySelector('[data-card="config.lookup_values"]') as HTMLElement;
  await fireEvent.click(card);
  expect(calls).toEqual(['config.lookup_values']);
  // With onSelect provided, the diagram delegates instead of mutating state.
  expect(state.selected).toBe(null);
});
