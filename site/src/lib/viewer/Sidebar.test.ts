import { it, expect, beforeAll } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Sidebar from './Sidebar.svelte';
import { createViewerState } from './state.svelte';
import type { SchemaModel } from './model';

beforeAll(() => {
  // @ts-expect-error jsdom lacks this; Rokkit Navigator calls it on select
  Element.prototype.scrollIntoView ??= () => {};
});

const MODEL: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
  ],
  refs: [],
};

it('renders a group per schema and an item per table', () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  expect(container.querySelectorAll('[data-list-group]').length).toBe(1);
  expect(container.querySelectorAll('[data-list-item]').length).toBe(2);
});

it('filter narrows the visible items', async () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  const input = container.querySelector('[data-filter]') as HTMLInputElement;
  await fireEvent.input(input, { target: { value: 'values' } });
  const items = container.querySelectorAll('[data-list-item]');
  expect(items.length).toBe(1);
  expect((items[0] as HTMLElement).textContent).toContain('lookup_values');
});

it('selecting an item sets state.selected and focus mode', async () => {
  const state = createViewerState();
  const { container } = render(Sidebar, { props: { model: MODEL, state } });
  const item = [...container.querySelectorAll('[data-list-item]')]
    .find((el) => el.textContent?.includes('lookup_values')) as HTMLElement;
  await fireEvent.click(item);
  expect(state.selected).toBe('config.lookup_values');
  expect(state.mode).toBe('focus');
});
