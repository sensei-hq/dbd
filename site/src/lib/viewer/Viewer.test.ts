import { it, expect, beforeAll } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Viewer from './Viewer.svelte';
import type { SchemaModel } from './model';

beforeAll(() => {
  // jsdom lacks these; Rokkit calls them (Sidebar's List→Navigator uses
  // scrollIntoView on select, ThemeSwitcherToggle resolves `system` via matchMedia).
  Element.prototype.scrollIntoView ??= () => {};
  if (!window.matchMedia) {
    window.matchMedia = (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {
        return false;
      },
    });
  }
});

const MODEL: SchemaModel = {
  project: { name: 'TestProj', db: 'postgresql' },
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

it('renders header (name + counts), sidebar, diagram; selecting a table opens detail', async () => {
  const { container } = render(Viewer, { props: { model: MODEL } });
  const header = container.querySelector('[data-viewer-header]')!;
  expect(header.textContent).toContain('TestProj');
  expect(header.textContent).toContain('2 tables');
  expect(container.querySelector('[data-list-group]')).toBeTruthy(); // sidebar
  expect(container.querySelectorAll('[data-card]').length).toBe(2); // diagram cards
  expect(container.querySelector('[data-detail]')).toBeNull(); // closed initially
  const item = [...container.querySelectorAll('[data-list-item]')].find((el) =>
    el.textContent?.includes('lookup_values')
  )!;
  await fireEvent.click(item);
  expect(container.querySelector('[data-detail]')).toBeTruthy(); // opens
});
