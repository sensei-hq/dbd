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

it('renders header (project name + counts), sidebar, diagram', () => {
  const { container } = render(Viewer, { props: { model: MODEL } });
  const header = container.querySelector('[data-viewer-header]')!;
  expect(header.textContent).toContain('TestProj'); // breadcrumb project name
  // design header (project root) shows the project name + the counts row
  expect(container.textContent).toContain('2 tables');
  expect(container.querySelector('[data-tree-group]')).toBeTruthy(); // sidebar
  expect(container.querySelectorAll('[data-card]').length).toBe(2); // diagram cards
});

it('clicking a sidebar table opens the EntityView (columns table + entity name)', async () => {
  const { container } = render(Viewer, { props: { model: MODEL } });
  // project root visible first → no per-column rows yet
  expect(container.querySelector('[data-col-row]')).toBeNull();
  const item = [...container.querySelectorAll('[data-tree-item]')].find((el) =>
    el.textContent?.includes('lookup_values')
  )!;
  await fireEvent.click(item);
  // EntityView is now mounted: its entity name + the columns table appear
  const h1 = container.querySelector('h1')!;
  expect(h1.textContent).toContain('lookup_values');
  expect(container.querySelectorAll('[data-col-row]').length).toBe(2); // id, lookup_id
  // diagram cards are gone (EntityView replaced the project root)
  expect(container.querySelector('[data-card]')).toBeNull();
});

it('switching to the Entities tab shows the entities table', async () => {
  const { container, getByText } = render(Viewer, { props: { model: MODEL } });
  await fireEvent.click(getByText('Entities'));
  const rows = container.querySelectorAll('[data-entity-row]');
  expect(rows.length).toBe(2); // one row per table
  expect(container.textContent).toContain('lookup_values');
});
