import { it, expect, describe } from 'vitest';
import { render, fireEvent, findByRole } from '@testing-library/svelte';
import EntityView from './EntityView.svelte';
import { entityHistory } from './changelog';
import { sampleModel } from './data';

// An entity's own changelog (#33): the project changelog narrowed to one table or enum.
// Every row is re-derived from `model.history`, so the page cannot drift from the project tab.

const history = sampleModel.history!;
const touching = (schema: string, name: string) =>
  history
    .filter((h) => h.changes.some((c) => c.schema === schema && c.name === name))
    .sort((a, b) => b.version - a.version);

describe('entityHistory', () => {
  it('lists the versions that changed this entity, newest first, with its own change', () => {
    const { rows } = entityHistory(history, 'table', 'shop', 'orders');
    expect(rows.map((r) => r.entry.version)).toEqual(touching('shop', 'orders').map((h) => h.version));
    for (const r of rows) {
      expect(r.change).toEqual(r.entry.changes.find((c) => c.schema === 'shop' && c.name === 'orders'));
    }
  });

  it('does not list a version that only touched other entities', () => {
    const versions = entityHistory(history, 'table', 'shop', 'orders').rows.map((r) => r.entry.version);
    expect(versions).not.toContain(3);
  });

  it('marks an entity added in a version, and one present since the baseline', () => {
    const items = entityHistory(history, 'table', 'shop', 'order_items');
    expect(items.rows.map((r) => [r.entry.version, r.change.op])).toEqual([[2, 'added']]);
    expect(items.sinceBaseline).toBeUndefined();

    const sessions = entityHistory(history, 'table', 'auth', 'sessions');
    expect(sessions.rows).toEqual([]);
    expect(sessions.sinceBaseline).toBe(1);
  });

  it('keeps a table and an enum of the same name apart', () => {
    expect(entityHistory(history, 'table', 'shop', 'order_status').rows).toEqual([]);
    expect(entityHistory(history, 'enum', 'shop', 'order_status').rows.map((r) => r.entry.version)).toEqual([5]);
  });

  it('is empty, with no baseline, for a model without history', () => {
    expect(entityHistory([], 'table', 'shop', 'orders')).toEqual({ rows: [] });
  });
});

describe("an entity's Changelog tab", () => {
  async function openChangelog(entityKey: string) {
    const view = render(EntityView, { props: { model: sampleModel, entityKey, onNav: () => {} } });
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Changelog' }));
    return view.container;
  }

  it('shows the versions that changed it, newest first, with what changed', async () => {
    const container = await openChangelog('shop.orders');
    const rows = [...container.querySelectorAll('[data-entity-changelog] [data-version]')];
    expect(rows.map((r) => Number(r.getAttribute('data-version')))).toEqual([5, 2]);
    expect(rows[0].textContent).toMatch(/numeric\(10,2\) → integer not null default 0/);
  });

  it('says an entity the history never touched has been there since the baseline', async () => {
    const container = await openChangelog('auth.sessions');
    expect(container.querySelector('[data-entity-changelog]')?.textContent).toMatch(/since v1/i);
  });

  it('explains an empty history', async () => {
    const { history: _h, ...noHistory } = sampleModel;
    const view = render(EntityView, { props: { model: noHistory, entityKey: 'shop.orders', onNav: () => {} } });
    await fireEvent.click(await findByRole(view.container, 'button', { name: 'Changelog' }));
    expect(view.container.querySelector('[data-entity-changelog]')?.textContent).toMatch(/no snapshots/i);
  });
});
