import { it, expect, describe } from 'vitest';
import { render, findAllByText, findByRole, fireEvent } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import { encodeFragment } from './fragment';
import type { SchemaModel } from './model';
import { vibe } from '@rokkit/states';
import { sampleModel } from './data';

// `[data-graph-node]` is @rokkit/graph's node hook — it replaced dbd's `[data-card]` when the
// viewer moved into the package. The assertion is unchanged in intent: the diagram rendered
// a card per table.

// The root opens on the project overview (#28); the diagram is one tab over.
const tick = () => new Promise((r) => setTimeout(r, 0));
const q = (root: Element, sel: string) => root.querySelector(sel);
const qa = (root: Element, sel: string) => root.querySelectorAll(sel);

async function openDiagram() {
  const view = render(Page);
  await tick();
  await fireEvent.click(await findByRole(view.container, 'button', { name: 'Diagram' }));
  await tick();
  return view;
}

it('renders the bundled sample diagram on the Diagram tab (no payload)', async () => {
  const { container } = await openDiagram();
  expect(container.querySelectorAll('[data-graph-node]').length).toBeGreaterThanOrEqual(2);
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
  const { container } = await openDiagram();
  // `widgets` comes from the decoded fragment, not the sample → proves decode ran.
  await findAllByText(container, 'widgets');
  expect(container.querySelectorAll('[data-graph-node]').length).toBeGreaterThanOrEqual(2);
  window.location.hash = '';
});

// A foreign key out of the model lands on a stub, drawn as a card of its own kind so it reads as
// "not one of this model's tables" — rather than the edge being dropped.
const stubbed: SchemaModel = {
  version: 3,
  project: { name: 'stubbed', db: 'postgresql' },
  schemas: [{ name: 'app', tables: 1, enums: 0 }],
  tables: [
    {
      schema: 'app',
      name: 'profiles',
      kind: 'table',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true },
        { name: 'user_id', type: 'uuid', fk: true },
      ],
    },
  ],
  refs: [{ from: { s: 'app', t: 'profiles', c: 'user_id' }, to: { s: 'auth', t: 'users', c: 'id' } }],
  stubs: [{ schema: 'auth', name: 'users', kind: 'external', columns: [{ name: 'id', type: 'uuid' }] }],
};

describe('a foreign key into a stub', () => {
  it('is drawn to a card that carries the stub\'s kind', async () => {
    window.location.hash = '#' + (await encodeFragment(stubbed));
    const { container } = await openDiagram();
    expect(q(container, '[data-graph-node="auth.users"]')?.getAttribute('data-node-kind')).toBe('external');
    expect(q(container, '[data-graph-edge][data-edge-to="auth.users"]')).not.toBeNull();
    window.location.hash = '';
  });

  it('appears in the neighbourhood of the table that references it', async () => {
    window.location.hash = '#' + (await encodeFragment(stubbed));
    const view = await openDiagram();
    await fireEvent.click(q(view.container, '[data-graph-node="app.profiles"]')!);
    await tick();
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Diagram' }));
    await tick();
    expect(q(view.container, '[data-graph-node="auth.users"]')?.getAttribute('data-node-kind')).toBe('external');
    window.location.hash = '';
  });

  it('is not listed in the sidebar as one of the model\'s tables', async () => {
    window.location.hash = '#' + (await encodeFragment(stubbed));
    const { container } = await openDiagram();
    expect(q(container, 'aside [data-entity-key="auth.users"]')).toBeNull();
    window.location.hash = '';
  });
});

// @rokkit/graph 1.7 made `Graph` a bare canvas: no density bar, no zoom buttons, and a default
// layout of `flow` instead of `cluster`. These pin what the viewer gets back by composing the
// package's named diagrams instead — `ErDiagram` at the root, `Neighborhood` on an entity.

describe('the root ER diagram', () => {
  it('ranks by reference direction and paints each card with its schema', async () => {
    const { container } = await openDiagram();
    expect(q(container, '[data-graph-layout]')?.getAttribute('data-graph-layout')).toBe('flow');
    // `flow` draws no schema boxes, so without the tint the schema a table belongs to is gone.
    expect(q(container, '[data-graph-paper]')?.hasAttribute('data-graph-group-tint')).toBe(true);
  });

  it('keys the schema tint in a legend', async () => {
    const { container } = await openDiagram();
    const entries = [...qa(container, '[data-graph-legend] [data-graph-legend-entry]')].map((e) =>
      e.textContent?.trim(),
    );
    expect(entries).toEqual(expect.arrayContaining(['auth', 'shop']));
  });

  it('has a density control that changes how many rows each card shows', async () => {
    const { container } = await openDiagram();
    const keyRows = qa(container, '[data-graph-row]').length;
    await fireEvent.click(q(container, '[data-graph-density="full"]')!);
    await tick();
    expect(qa(container, '[data-graph-row]').length).toBeGreaterThan(keyRows);
  });

  it('has zoom buttons that move the zoom off fit', async () => {
    const { container } = await openDiagram();
    const reset = q(container, '[data-graph-zoom="reset"]')!;
    expect(reset.textContent?.trim()).toBe('100%');
    await fireEvent.click(q(container, '[data-graph-zoom="in"]')!);
    await tick();
    expect(reset.textContent?.trim()).toBe('125%');
  });

  it('has an edge-style toggle', async () => {
    const { container } = await openDiagram();
    const toggle = q(container, '[data-graph-edge-style]')!;
    expect(toggle.getAttribute('data-graph-edge-style')).toBe('curved');
    await fireEvent.click(toggle);
    await tick();
    expect(q(container, '[data-graph-edge-style]')?.getAttribute('data-graph-edge-style')).toBe(
      'orthogonal',
    );
  });

  it('opens a table when its card is clicked', async () => {
    const { container } = await openDiagram();
    await fireEvent.click(q(container, '[data-graph-node="shop.orders"]')!);
    await tick();
    expect(q(container, 'h1')?.textContent?.trim()).toBe('orders');
  });
});

describe("an entity's relationship diagram", () => {
  async function openDiagramTab(entity: string) {
    const view = await openDiagram();
    await fireEvent.click(q(view.container, `[data-graph-node="${entity}"]`)!);
    await tick();
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Diagram' }));
    await tick();
    return view.container;
  }

  it('centres the entity in its neighbourhood', async () => {
    const container = await openDiagramTab('shop.orders');
    expect(q(container, '[data-graph-layout]')?.getAttribute('data-graph-layout')).toBe(
      'neighborhood',
    );
    expect(q(container, '[data-graph-node="shop.orders"]')).not.toBeNull();
  });

  it('carries the zoom and depth controls the canvas no longer draws', async () => {
    const container = await openDiagramTab('shop.orders');
    expect(q(container, '[data-graph-zoom-controls]')).not.toBeNull();
    expect(q(container, '[data-graph-depth-controls]')).not.toBeNull();
  });

  it('walks to a neighbour when its card is clicked', async () => {
    const container = await openDiagramTab('shop.orders');
    await fireEvent.click(q(container, '[data-graph-node="shop.customers"]')!);
    await tick();
    expect(q(container, 'h1')?.textContent?.trim()).toBe('customers');
  });

  // The root's look, carried over: schema tint on every card and no selection highlight at
  // rest. Before rokkit 1.8.1 a selected focus outlined every neighbour as `related` and
  // dimmed the second ring to 0.3 — the ring a reader asked to see (rokkit#172).

  it('paints each card with its schema, as the root diagram does', async () => {
    const container = await openDiagramTab('shop.orders');
    expect(q(container, '[data-graph-paper]')?.hasAttribute('data-graph-group-tint')).toBe(true);
  });

  it('shows the second ring at full strength, with no card selected, related or dim', async () => {
    const container = await openDiagramTab('shop.customers');
    const oneHop = qa(container, '[data-graph-node]').length;
    await fireEvent.click(q(container, '[data-graph-depth="2"]')!);
    await tick();
    expect(qa(container, '[data-graph-node]').length).toBeGreaterThan(oneHop);
    expect(qa(container, '[data-graph-node][data-node-state]').length).toBe(0);
  });

  // rokkit#170: 1.8.1 reserved an empty column on the side with no neighbours to centre the
  // focus, which pushed the drawn cards of a table nothing references (order_items, sessions)
  // off to one side. The cards are what is centred; nothing is reserved for an empty side.
  it('reserves no empty column on a side with no neighbours', async () => {
    for (const entity of ['shop.order_items', 'auth.users']) {
      const container = await openDiagramTab(entity);
      const world = q(container, '[data-graph-world]') as HTMLElement;
      const cards = [...qa(container, '[data-graph-node]')].map((n) => {
        const s = (n as HTMLElement).style;
        return { left: parseFloat(s.left), right: parseFloat(s.left) + parseFloat(s.width) };
      });
      const firstLeft = Math.min(...cards.map((c) => c.left));
      const lastRight = Math.max(...cards.map((c) => c.right));
      expect(firstLeft, `${entity}: nothing reserved before the first card`).toBeLessThan(40);
      expect(parseFloat(world.style.width) - lastRight, `${entity}: nothing reserved after the last`).toBeLessThan(40);
    }
  });

  it('marks only the focus when its own card is clicked — no neighbour related or dim', async () => {
    const container = await openDiagramTab('shop.orders');
    await fireEvent.click(q(container, '[data-graph-node="shop.orders"]')!);
    await tick();
    expect(q(container, 'h1')?.textContent?.trim()).toBe('orders');
    const states = [...qa(container, '[data-graph-node][data-node-state]')].map((n) => [
      n.getAttribute('data-graph-node'),
      n.getAttribute('data-node-state'),
    ]);
    expect(states).toEqual([['shop.orders', 'selected']]);
  });
});

describe('the app header', () => {
  // The marketing nav's switcher, not a page-local moon/sun button — one control for colour
  // mode across the whole site.
  it('switches colour mode with the same ThemeSwitcherToggle as the home page', async () => {
    vibe.mode = 'light';
    const { container } = render(Page);
    await tick();
    const toggle = q(container, 'header [data-toggle][data-toggle-variant="button"]');
    expect(toggle).not.toBeNull();
    await fireEvent.click(toggle!);
    await tick();
    expect(vibe.mode).toBe('dark');
    vibe.mode = 'light';
  });
});

describe('the root overview', () => {
  it('is where the page opens, with tabs Overview, Diagram, Entities, Changelog', async () => {
    const { container } = render(Page);
    await tick();
    expect(q(container, '[data-overview]')).not.toBeNull();
    expect(q(container, '[data-graph-node]')).toBeNull();
    const tabs = [...container.querySelectorAll('button')]
      .map((b) => b.textContent?.trim())
      .filter((t) => ['Overview', 'Diagram', 'Entities', 'Changelog'].includes(t ?? ''));
    expect(tabs).toEqual(['Overview', 'Diagram', 'Entities', 'Changelog']);
  });

  it('lists the latest versions under Recent changes, and opens the changelog from there', async () => {
    const { container } = render(Page);
    await tick();
    const recent = q(container, '[data-overview] [data-section="recent"]')!;
    const newest = Math.max(...sampleModel.history!.map((h) => h.version));
    expect(recent.querySelector('[data-version]')?.getAttribute('data-version')).toBe(String(newest));
    expect(recent.querySelectorAll('[data-version]').length).toBeLessThanOrEqual(3);
    await fireEvent.click(await findByRole(recent as HTMLElement, 'button', { name: /changelog/i }));
    await tick();
    expect(q(container, '[data-changelog]')).not.toBeNull();
  });

  it('shows a tile per count, with its icon', async () => {
    const { container } = render(Page);
    await tick();
    const tile = q(container, '[data-count="tables"]')!;
    expect(tile.textContent?.replace(/\s+/g, ' ')).toContain('6 Tables');
    expect(tile.querySelector('[data-count-icon]')?.className).toContain('i-glyph:table');
  });

  it('keeps the header as it is on every tab — the subtitle and the stats stay put', async () => {
    const { container } = render(Page);
    await tick();
    const subtitle = () =>
      [...container.querySelectorAll('p')].filter((p) => p.textContent?.includes('Storefront catalog')).length;
    const stats = () => container.textContent?.replace(/\s+/g, ' ').includes('6 tables');
    expect(subtitle()).toBe(1);
    expect(stats()).toBe(true);
    await fireEvent.click(await findByRole(container, 'button', { name: 'Diagram' }));
    await tick();
    expect(subtitle()).toBe(1);
    expect(stats()).toBe(true);
  });

  it('says where a note comes from when the project has none', async () => {
    const { note: _n, ...project } = sampleModel.project;
    window.location.hash = '#' + (await encodeFragment({ ...sampleModel, project }));
    const view = render(Page);
    await findAllByText(view.container, 'shopdb');
    await tick();
    expect(q(view.container, '[data-overview] [data-section="notes"]')?.textContent).toMatch(/project\.note/);
    window.location.hash = '';
  });

  it('leaves a one-line note to the subtitle rather than printing it twice', async () => {
    const { container } = render(Page);
    await tick();
    expect(q(container, '[data-overview] [data-section="notes"]')).toBeNull();
  });

  it('renders a longer note in full under Notes, with its first line as the subtitle', async () => {
    const note = 'Storefront catalog.\n- customers and their orders\n- products and stock';
    window.location.hash = '#' + (await encodeFragment({ ...sampleModel, project: { ...sampleModel.project, note } }));
    const view = render(Page);
    await findAllByText(view.container, 'products and stock');
    const notes = q(view.container, '[data-overview] [data-section="notes"]')!;
    expect([...notes.querySelectorAll('li')].map((li) => li.textContent?.trim())).toEqual([
      'customers and their orders',
      'products and stock',
    ]);
    // In the header, not the overview's own first paragraph.
    const subtitle = [...view.container.querySelectorAll('p')].find(
      (p) => !p.closest('[data-overview]') && p.textContent?.trim() === 'Storefront catalog.',
    );
    expect(subtitle).toBeDefined();
    window.location.hash = '';
  });
});

describe('the changelog', () => {
  async function openChangelog() {
    const view = render(Page);
    await tick();
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Changelog' }));
    await tick();
    return view.container;
  }

  it('shows one card per version, newest first', async () => {
    const container = await openChangelog();
    const versions = [...container.querySelectorAll('[data-changelog] [data-version]')].map((v) =>
      Number(v.getAttribute('data-version')),
    );
    expect(versions).toEqual(sampleModel.history!.map((h) => h.version).sort((a, b) => b - a));
  });

  it('lists each changed entity with what happened to it', async () => {
    const container = await openChangelog();
    const v2 = q(container, '[data-changelog] [data-version="2"]')!;
    const rows = [...v2.querySelectorAll('[data-change]')].map((r) => ({
      id: r.getAttribute('data-change'),
      op: r.getAttribute('data-op'),
    }));
    expect(rows).toEqual(
      sampleModel.history!
        .find((h) => h.version === 2)!
        .changes.map((c) => ({ id: `${c.schema}.${c.name}`, op: c.op })),
    );
  });

  it('spells out a rename and a modified column inside their entity', async () => {
    const container = await openChangelog();
    const text = q(container, '[data-changelog]')!.textContent!.replace(/\s+/g, ' ');
    expect(text).toMatch(/display_name → name/);
    expect(text).toMatch(/numeric\(10,2\) → integer not null default 0/);
  });

  it('shows the baseline as counts, not a list', async () => {
    const container = await openChangelog();
    const base = q(container, '[data-changelog] [data-version="1"]')!;
    expect(base.querySelectorAll('[data-change]')).toHaveLength(0);
    expect(base.textContent).toMatch(/5 tables/);
  });

  it('explains an empty changelog instead of showing nothing', async () => {
    const { history: _h, ...noHistory } = sampleModel;
    window.location.hash = '#' + (await encodeFragment(noHistory));
    const view = render(Page);
    await findAllByText(view.container, 'shopdb');
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Changelog' }));
    await tick();
    expect(q(view.container, '[data-changelog]')?.textContent).toMatch(/no snapshots/i);
    window.location.hash = '';
  });
});
