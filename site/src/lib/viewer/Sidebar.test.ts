import { it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Sidebar from './Sidebar.svelte';
import { createViewerState } from './state.svelte';
import type { SchemaModel } from './model';

const MODEL: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
  ],
  refs: [],
};

it('renders a group head per schema and an item per table', () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  expect(container.querySelectorAll('[data-tree-group]').length).toBe(1);
  expect(container.querySelectorAll('[data-tree-item]').length).toBe(2);
});

it('filter narrows the visible items', async () => {
  const state = createViewerState();
  const { container, getByPlaceholderText } = render(Sidebar, { props: { model: MODEL, state } });
  const input = getByPlaceholderText('Find an entity…') as HTMLInputElement;
  await fireEvent.input(input, { target: { value: 'values' } });
  const items = container.querySelectorAll('[data-tree-item]');
  expect(items.length).toBe(1);
  expect((items[0] as HTMLElement).textContent).toContain('lookup_values');
});

it('a filter with no matches shows the empty message', async () => {
  const state = createViewerState();
  const { container, getByPlaceholderText } = render(Sidebar, { props: { model: MODEL, state } });
  const input = getByPlaceholderText('Find an entity…') as HTMLInputElement;
  await fireEvent.input(input, { target: { value: 'zzz-nope' } });
  expect(container.querySelectorAll('[data-tree-group]').length).toBe(0);
  expect(container.textContent).toContain('Nothing matches');
});

it('selecting an item sets state.selected and focus mode', async () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  const item = [...container.querySelectorAll('[data-tree-item]')].find((el) =>
    el.textContent?.includes('lookup_values')
  ) as HTMLElement;
  await fireEvent.click(item);
  expect(state.selected).toBe('config.lookup_values');
  expect(state.mode).toBe('focus');
});

it('clicking a group head collapses its items', async () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  expect(container.querySelectorAll('[data-tree-item]').length).toBe(2);
  const head = container.querySelector('[data-tree-group]') as HTMLElement;
  await fireEvent.click(head);
  expect(container.querySelectorAll('[data-tree-item]').length).toBe(0);
});
