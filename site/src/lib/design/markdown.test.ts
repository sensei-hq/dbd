import { it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import EntitiesView from './EntitiesView.svelte';
import type { SchemaModel } from './model';

// A comment may name the same `code` twice — legal text that the entities list keyed by its
// own content and crashed on with each_key_duplicate. Every comment on the site goes through
// one renderer now, keyed by position.

it('renders a table comment that names the same code twice in the entities list', () => {
  const model: SchemaModel = {
    project: { name: 'p', db: 'postgresql' },
    schemas: [{ name: 'app', tables: 1, enums: 0 }],
    tables: [
      {
        schema: 'app',
        name: 'copies',
        kind: 'table',
        noteMd: 'Mirrors `id`, never `id` itself.',
        columns: [{ name: 'id', type: 'int' }],
      },
    ],
    refs: [],
  };
  const { container } = render(EntitiesView, { props: { model } });
  expect([...container.querySelectorAll('code')].map((c) => c.textContent)).toEqual(['id', 'id']);
});
