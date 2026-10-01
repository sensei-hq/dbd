import { it, expect, describe, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import EntityView from './EntityView.svelte';
import type { SchemaModel } from './model';

// The Details tab, laid out the way a reader scans a table's documentation: what it is, its
// fields, what it links to, and what depends on it (#27).

const model: SchemaModel = {
  version: 2,
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'app', tables: 3, enums: 1 }],
  tables: [
    {
      schema: 'app',
      name: 'orders',
      kind: 'table',
      noteMd: 'A placed order.\n- belongs to one customer',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true, note: 'Primary key.' },
        { name: 'customer_id', type: 'uuid', nn: true, fk: true },
        {
          name: 'status',
          type: 'order_status',
          nn: true,
          en: true,
          def: 'pending',
          note: 'Lifecycle state:\n- `pending` until paid\n- `shipped` once dispatched',
        },
        { name: 'sku', type: 'varchar(32)', nn: true, uq: true },
      ],
      indexes: [{ def: '(sku)', unique: true, name: 'orders_sku_key' }],
    },
    { schema: 'app', name: 'customers', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    {
      schema: 'app',
      name: 'order_items',
      kind: 'table',
      columns: [{ name: 'order_id', type: 'uuid', nn: true }],
    },
  ],
  refs: [
    { from: { s: 'app', t: 'orders', c: 'customer_id' }, to: { s: 'app', t: 'customers', c: 'id' }, action: 'cascade' },
    { from: { s: 'app', t: 'order_items', c: 'order_id' }, to: { s: 'app', t: 'orders', c: 'id' } },
  ],
  entities: [
    { schema: 'app', name: 'order_totals', kind: 'view' },
    { schema: 'app', name: 'log_order', kind: 'function' },
  ],
  deps: [
    { from: { s: 'app', n: 'order_totals' }, to: { s: 'app', n: 'orders' }, kind: 'reads' },
    { from: { s: 'app', n: 'log_order' }, to: { s: 'app', n: 'orders' }, kind: 'writes' },
    { from: { s: 'app', n: 'order_totals' }, to: { s: 'app', n: 'customers' }, kind: 'reads' },
  ],
};

const open = (m: SchemaModel = model, entityKey = 'app.orders') => {
  const onNav = vi.fn();
  const view = render(EntityView, { props: { model: m, entityKey, onNav } });
  return { ...view, onNav };
};
const text = (el: Element | null | undefined) => el?.textContent?.replace(/\s+/g, ' ').trim();
const row = (root: Element, col: string) => root.querySelector(`[data-col-row="${col}"]`)!;
const cell = (root: Element, col: string, name: string) =>
  row(root, col).querySelector(`[data-cell="${name}"]`)!;

describe('the Details tab', () => {
  it('reads top to bottom: table info, fields, references, dependencies, indexes', () => {
    const { container } = open();
    const sections = [...container.querySelectorAll('[data-section]')].map((s) =>
      s.getAttribute('data-section'),
    );
    expect(sections).toEqual(['info', 'fields', 'references', 'dependencies', 'indexes']);
  });

  it('opens with the table comment, rendered as markdown', () => {
    const { container } = open();
    const info = container.querySelector('[data-section="info"]')!;
    expect(text(info.querySelector('p'))).toBe('A placed order.');
    expect([...info.querySelectorAll('li')].map(text)).toEqual(['belongs to one customer']);
  });

  it('says so when the table has no comment, rather than dropping the section', () => {
    const { container } = open(model, 'app.customers');
    expect(text(container.querySelector('[data-section="info"]'))).toMatch(/no comment/i);
  });
});

describe('the Fields table', () => {
  it('has Name, Type, Settings, Default, References and Notes columns', () => {
    const { container } = open();
    const heads = [...container.querySelectorAll('[data-section="fields"] th')].map(text);
    expect(heads).toEqual(['Name', 'Type', 'Settings', 'Default', 'References', 'Notes']);
  });

  it('keeps the Name cell to the name alone', () => {
    const { container } = open();
    expect(text(cell(container, 'status', 'name'))).toBe('status');
  });

  it('shows the full declared type', () => {
    const { container } = open();
    expect(text(cell(container, 'sku', 'type'))).toBe('varchar(32)');
  });

  it('badges every setting the column carries', () => {
    const { container } = open();
    const badges = (col: string) =>
      [...cell(container, col, 'settings').querySelectorAll('[data-badge]')].map(text);
    expect(badges('id')).toEqual(['PK', 'NN']);
    expect(badges('customer_id')).toEqual(['FK', 'NN']);
    expect(badges('status')).toEqual(['NN', 'ENUM']);
    expect(badges('sku')).toEqual(['NN', 'UNIQUE']);
  });

  it('gives the default its own column, with a dash where there is none', () => {
    const { container } = open();
    expect(text(cell(container, 'status', 'default'))).toBe('pending');
    expect(text(cell(container, 'id', 'default'))).toBe('—');
  });

  it('links a foreign key to the table it references', async () => {
    const { container, onNav } = open();
    const link = cell(container, 'customer_id', 'refs').querySelector('button')!;
    expect(text(link)).toBe('→ app.customers.id');
    await fireEvent.click(link);
    expect(onNav).toHaveBeenCalledWith('app.customers');
  });

  it('renders a markdown note — bullets as a list, `code` as code', () => {
    const { container } = open();
    const notes = cell(container, 'status', 'notes');
    expect(text(notes.querySelector('p'))).toBe('Lifecycle state:');
    expect(notes.querySelectorAll('li')).toHaveLength(2);
    expect([...notes.querySelectorAll('code')].map(text)).toEqual(['pending', 'shipped']);
  });
});

describe('the References section', () => {
  it('lists outgoing and incoming foreign keys, each one navigable', async () => {
    const { container, onNav } = open();
    const refs = container.querySelector('[data-section="references"]')!;
    const outgoing = [...refs.querySelectorAll('[data-ref="out"]')].map(text);
    const incoming = [...refs.querySelectorAll('[data-ref="in"]')].map(text);
    expect(outgoing).toEqual([expect.stringContaining('customer_id → app.customers.id')]);
    expect(outgoing[0]).toContain('cascade');
    expect(incoming).toEqual([expect.stringContaining('app.order_items.order_id → id')]);
    await fireEvent.click(refs.querySelector('[data-ref="in"]')!);
    expect(onNav).toHaveBeenCalledWith('app.order_items');
  });
});

describe('the Dependencies section', () => {
  it('lists what reads and writes the table, by kind', () => {
    const { container } = open();
    const deps = [...container.querySelectorAll('[data-section="dependencies"] [data-dep]')].map((d) => ({
      id: d.getAttribute('data-dep'),
      text: text(d),
    }));
    expect(deps).toEqual([
      { id: 'app.order_totals', text: expect.stringMatching(/order_totals.*view.*reads/) },
      { id: 'app.log_order', text: expect.stringMatching(/log_order.*function.*writes/) },
    ]);
  });

  it('says so when nothing depends on the table', () => {
    const { container } = open(model, 'app.order_items');
    expect(text(container.querySelector('[data-section="dependencies"]'))).toMatch(/nothing/i);
  });

  it('is omitted for a v1 model, which carries no dependency graph', () => {
    const { entities: _e, deps: _d, version: _v, ...v1 } = model;
    const { container } = open(v1);
    expect(container.querySelector('[data-section="dependencies"]')).toBeNull();
  });
});
