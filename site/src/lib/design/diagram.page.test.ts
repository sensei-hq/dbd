import { it, expect, describe } from 'vitest';
import { render, findAllByText, findByRole, fireEvent } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import { encodeFragment } from './fragment';
import type { SchemaModel } from './model';
import { vibe } from '@rokkit/states';

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

  // The root's look, carried over: schema tint on every card and no selection highlight.
  // `Neighborhood` has no `groupTint` prop, and a selected focus outlines every neighbour as
  // `related` — and dims the second ring to 0.3, which is the ring a reader asked to see.

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

  it('leaves no highlight behind when the focus card itself is clicked', async () => {
    const container = await openDiagramTab('shop.orders');
    await fireEvent.click(q(container, '[data-graph-node="shop.orders"]')!);
    await tick();
    expect(q(container, 'h1')?.textContent?.trim()).toBe('orders');
    expect(qa(container, '[data-graph-node][data-node-state]').length).toBe(0);
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
  it('is where the page opens, with tabs Overview, Diagram, Entities', async () => {
    const { container } = render(Page);
    await tick();
    expect(q(container, '[data-overview]')).not.toBeNull();
    expect(q(container, '[data-graph-node]')).toBeNull();
    const tabs = [...container.querySelectorAll('button')]
      .map((b) => b.textContent?.trim())
      .filter((t) => ['Overview', 'Diagram', 'Entities'].includes(t ?? ''));
    expect(tabs).toEqual(['Overview', 'Diagram', 'Entities']);
  });

  it('shows a tile per count, with its icon', async () => {
    const { container } = render(Page);
    await tick();
    const tile = q(container, '[data-count="tables"]')!;
    expect(tile.textContent?.replace(/\s+/g, ' ')).toContain('6 Tables');
    expect(tile.querySelector('[data-count-icon]')?.className).toContain('i-glyph:table');
  });

  it('renders the project note in full, and drops the header copy of it', async () => {
    const { container } = render(Page);
    await tick();
    const notes = q(container, '[data-overview] [data-section="notes"]')!;
    expect(notes.textContent).toContain('Storefront catalog, customers and orders.');
    const header = [...container.querySelectorAll('p')].filter((p) =>
      p.textContent?.includes('Storefront catalog'),
    );
    expect(header).toHaveLength(1);
  });
});
