import { it, expect, describe } from 'vitest';
import { render } from '@testing-library/svelte';
import EntityView from './EntityView.svelte';
import EntitiesView from './EntitiesView.svelte';
import type { SchemaModel } from './model';

// A qualified name reads as one name — `auth.users` — not as a schema on one line and a table
// under it. Both views stacked two block elements, so the schema always broke onto its own
// line whatever the width.

const model: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'auth', tables: 1, enums: 0 }],
  tables: [
    { schema: 'auth', name: 'users', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true, nn: true }] },
  ],
  refs: [],
};

const byText = (root: Element, text: string) =>
  [...root.querySelectorAll('*')].find((el) => el.children.length === 0 && el.textContent?.trim() === text);

describe('a qualified table name', () => {
  it('sits on one line with the schema in the entity header', () => {
    const { container } = render(EntityView, { props: { model, entityKey: 'auth.users', onNav: () => {} } });
    const name = container.querySelector('h1')!;
    expect(name.textContent?.trim()).toBe('users');
    // The schema prefix is the name's own lead-in, in the same row — not a line above it.
    expect(name.previousElementSibling?.textContent?.trim()).toBe('auth.');
  });

  it('sits on one line with the schema in the entities list', () => {
    const { container } = render(EntitiesView, { props: { model, onNav: () => {} } });
    const schema = byText(container, 'auth.')!;
    const name = byText(container, 'users')!;
    expect(getComputedStyle(schema).display).toBe('inline');
    expect(getComputedStyle(name).display).toBe('inline');
    expect(schema.parentElement).toBe(name.parentElement);
  });
});
