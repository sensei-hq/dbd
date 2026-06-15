import { it, expect } from 'vitest';
import { render, findAllByText } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import { encodeFragment } from './fragment';
import type { SchemaModel } from './model';

it('renders the bundled sample diagram by default (no payload)', async () => {
  const { container } = render(Page);
  await new Promise((r) => setTimeout(r, 0));
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
});

it('renders a model decoded from the URL fragment', async () => {
  const model: SchemaModel = {
    project: { name: 'frag', db: 'postgresql' },
    schemas: [{ name: 'app', tables: 2, enums: 0 }],
    tables: [
      { schema: 'app', name: 'widgets', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true, nn: true }] },
      { schema: 'app', name: 'gadgets', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true, nn: true }] },
    ],
    refs: [],
  };
  window.location.hash = '#' + (await encodeFragment(model));
  const { container } = render(Page);
  // `widgets` comes from the decoded fragment, not the sample → proves decode ran.
  await findAllByText(container, 'widgets');
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
  window.location.hash = '';
});
