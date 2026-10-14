import { it, expect, describe } from 'vitest';
import { render, fireEvent, findByRole } from '@testing-library/svelte';
import Sidebar from './Sidebar.svelte';
import DiagramView from './DiagramView.svelte';
import EntityView from './EntityView.svelte';
import ObjectView from './ObjectView.svelte';
import EnumView from './EnumView.svelte';
import ChangelogView from './ChangelogView.svelte';
import OverviewView from './OverviewView.svelte';
import { validateModel, type HistoryEntry, type SchemaModel } from './model';
import { entityHistory } from './changelog';
import { sampleModel } from './data';

// A workspace's model (#7 phase 3, slice 5): `modules` in load order, and each table, entity
// and enum naming the module that declares it. The viewer groups by module.

const workspace: SchemaModel = {
  version: 3,
  project: { name: 'shop', db: 'postgresql' },
  modules: [
    { name: 'shared', owner: 'shop/shared', note: 'Reference data.' },
    { name: 'core', owner: 'shop/core', depends_on: ['shared'] },
  ],
  schemas: [
    { name: 'app', tables: 2, enums: 1 },
    { name: 'ref', tables: 1, enums: 0 },
  ],
  tables: [
    {
      schema: 'app',
      name: 'orders',
      kind: 'table',
      module: 'core',
      columns: [
        { name: 'id', type: 'integer', pk: true },
        { name: 'currency', type: 'text', fk: true },
      ],
    },
    { schema: 'app', name: 'currencies', kind: 'table', module: 'shared', columns: [{ name: 'code', type: 'text', pk: true }] },
    { schema: 'ref', name: 'countries', kind: 'table', module: 'shared', columns: [{ name: 'code', type: 'text', pk: true }] },
  ],
  refs: [{ from: { s: 'app', t: 'orders', c: 'currency' }, to: { s: 'app', t: 'currencies', c: 'code' } }],
  entities: [{ schema: 'app', name: 'order_totals', kind: 'view', module: 'core' }],
  enums: [{ schema: 'app', name: 'status', values: ['open', 'closed'], module: 'shared' }],
  history: [
    { version: 1, description: 'First version', timestamp: '2026-10-01T00:00:00Z', module: 'shared', changes: [] },
    { version: 1, description: 'First version', timestamp: '2026-10-02T00:00:00Z', module: 'core', changes: [] },
  ],
};

const attr = (els: Iterable<Element>, name: string) => [...els].map((e) => e.getAttribute(name));

describe('a workspace model', () => {
  it('is read with its modules', () => {
    expect(validateModel(workspace)).toEqual({ ok: true, model: workspace });
  });

  it('is refused when its modules are not a list', () => {
    const bad = { ...workspace, modules: { shared: {} } };
    expect(validateModel(bad)).toEqual({ ok: false, error: 'modules must be an array of named modules' });
  });

  it('is refused when a module has no name', () => {
    for (const modules of [[null], [{}], [{ name: 3 }]]) {
      expect(validateModel({ ...workspace, modules })).toEqual({
        ok: false,
        error: 'modules must be an array of named modules',
      });
    }
  });
});

describe('the sidebar of a workspace', () => {
  const open = (model: SchemaModel) =>
    render(Sidebar, { props: { data: { project: { name: model.project.name }, model } } }).container;

  it('groups by module, in load order, then by schema', () => {
    const container = open(workspace);
    const modules = container.querySelectorAll('[data-module-group]');
    expect(attr(modules, 'data-module-group')).toEqual(['shared', 'core']);
    const [shared, core] = [...modules];
    expect(attr(shared.querySelectorAll('[data-schema-group]'), 'data-schema-group')).toEqual(['app', 'ref']);
    expect(attr(shared.querySelectorAll('[data-entity-key]'), 'data-entity-key')).toEqual([
      'app.currencies',
      'app.status',
      'ref.countries',
    ]);
    expect(attr(core.querySelectorAll('[data-entity-key]'), 'data-entity-key')).toEqual([
      'app.orders',
      'app.order_totals',
    ]);
  });

  it('groups a project by schema alone', () => {
    const container = open(sampleModel);
    expect(container.querySelector('[data-module-group]')).toBeNull();
    expect(container.querySelector('[data-schema-group]')).not.toBeNull();
  });
});

describe('the diagram of a workspace', () => {
  const legend = (container: Element) => attr(container.querySelectorAll('[data-legend-group]'), 'data-legend-group');

  it('is tinted by module, and by schema at the switch', async () => {
    const { container } = render(DiagramView, { props: { model: workspace } });
    const byModule = container.querySelector('[data-tint="module"]')!;
    const bySchema = container.querySelector('[data-tint="schema"]')!;
    expect(byModule.getAttribute('aria-pressed')).toBe('true');
    expect(legend(container).sort()).toEqual(['core', 'shared']);

    await fireEvent.click(bySchema);
    expect(bySchema.getAttribute('aria-pressed')).toBe('true');
    expect(legend(container).sort()).toEqual(['app', 'ref']);
  });

  it('of a project has no switch: it is tinted by schema', () => {
    const { container } = render(DiagramView, { props: { model: sampleModel } });
    expect(container.querySelector('[data-tint]')).toBeNull();
  });
});

describe("an entity's page in a workspace", () => {
  const page = (view: typeof EntityView | typeof ObjectView | typeof EnumView, entityKey: string) =>
    render(view, { props: { model: workspace, entityKey, onNav: () => {} } }).container;

  it('names its module', () => {
    expect(page(EntityView, 'app.orders').querySelector('[data-module]')?.getAttribute('data-module')).toBe('core');
    expect(page(ObjectView, 'app.order_totals').querySelector('[data-module]')?.getAttribute('data-module')).toBe(
      'core',
    );
    expect(page(EnumView, 'app.status').querySelector('[data-module]')?.getAttribute('data-module')).toBe('shared');
  });

  it("of a project's names none", () => {
    const { container } = render(EntityView, { props: { model: sampleModel, entityKey: 'shop.orders', onNav: () => {} } });
    expect(container.querySelector('[data-module]')).toBeNull();
  });
});

describe("a workspace's changelog", () => {
  it("lists each module's versions, each naming its module", () => {
    const { container } = render(ChangelogView, { props: { model: workspace } });
    const cards = [...container.querySelectorAll('[data-version]')];
    expect(cards.map((c) => [c.getAttribute('data-version'), c.getAttribute('data-module')])).toEqual([
      ['1', 'shared'],
      ['1', 'core'],
    ]);
    expect(cards[1].querySelector('[data-module-badge]')?.textContent?.trim()).toBe('core');
  });
});

describe("a workspace's history", () => {
  const entry = (module: string, version: number, baseline = false): HistoryEntry => ({
    version,
    description: `${module} v${version}`,
    timestamp: '2026-10-01T00:00:00Z',
    module,
    changes: [],
    ...(baseline ? { baseline: { tables: 1, enums: 0 } } : {}),
  });

  it("reads each module's versions newest first, the modules in depends_on order", () => {
    const history = [entry('shared', 1, true), entry('core', 1, true), entry('core', 2)];
    const { container } = render(ChangelogView, { props: { model: { ...workspace, history } } });
    const cards = [...container.querySelectorAll('[data-version]')];
    expect(cards.map((c) => `${c.getAttribute('data-module')} v${c.getAttribute('data-version')}`)).toEqual([
      'shared v1',
      'core v2',
      'core v1',
    ]);
  });

  it("dates an entity from its own module's baseline", async () => {
    const history = [entry('shared', 1, true), entry('core', 3, true)];
    expect(entityHistory(history, 'table', 'app', 'orders', 'core').sinceBaseline).toBe(3);
    const { container } = render(EntityView, {
      props: { model: { ...workspace, history }, entityKey: 'app.orders', onNav: () => {} },
    });
    await fireEvent.click(await findByRole(container, 'button', { name: 'Changelog' }));
    expect(container.querySelector('[data-entity-changelog]')?.textContent).toContain('Present since v3, the baseline');
  });

  it('says when its module has no snapshots, though another module has', async () => {
    const history = [entry('shared', 1, true)];
    const { container } = render(EntityView, {
      props: { model: { ...workspace, history }, entityKey: 'app.orders', onNav: () => {} },
    });
    await fireEvent.click(await findByRole(container, 'button', { name: 'Changelog' }));
    expect(container.querySelector('[data-entity-changelog]')?.textContent).toContain(
      'No snapshots of module core yet',
    );
  });
});

describe("a workspace's overview", () => {
  const entry = (module: string, version: number, timestamp: string): HistoryEntry => ({
    version,
    description: `${module} v${version}`,
    timestamp,
    module,
    changes: [],
  });

  it('shows the newest versions across the modules, each naming its module', () => {
    const history = [
      entry('shared', 1, '2026-10-01T00:00:00Z'),
      entry('core', 1, '2026-10-02T00:00:00Z'),
      entry('core', 2, '2026-10-03T00:00:00Z'),
      entry('shared', 2, '2026-10-04T00:00:00Z'),
    ];
    const { container } = render(OverviewView, { props: { model: { ...workspace, history } } });
    const rows = [...container.querySelectorAll('[data-section="recent"] [data-version]')];
    expect(rows.map((r) => `${r.getAttribute('data-module')} v${r.getAttribute('data-version')}`)).toEqual([
      'shared v2',
      'core v2',
      'core v1',
    ]);
    expect(rows.map((r) => r.querySelector('[data-module-badge]')?.textContent?.trim())).toEqual([
      'shared',
      'core',
      'core',
    ]);
  });
});
