import { it, expect, describe, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Sidebar from './Sidebar.svelte';
import { sampleModel } from './data';

// The sidebar lists every entity and filters by entity type (#34).

const open = () => {
  const onPick = vi.fn();
  const view = render(Sidebar, { props: { data: { project: { name: 'shopdb' }, model: sampleModel }, onPick } });
  return { ...view, onPick };
};
const listed = (root: Element) =>
  [...root.querySelectorAll('[data-entity-key]')].map((e) => `${e.getAttribute('data-kind')}:${e.getAttribute('data-entity-key')}`);

describe('the sidebar', () => {
  it('lists views, routines and enums beside the tables, each with its kind', () => {
    const { container } = open();
    expect(listed(container)).toEqual(
      expect.arrayContaining(['view:shop.order_totals', 'procedure:shop.place_order', 'enum:shop.order_status', 'table:shop.orders']),
    );
  });

  it('offers a filter for each kind the project has, with its count', () => {
    const { container } = open();
    const chips = [...container.querySelectorAll('[data-kind-filter]')].map((c) => [
      c.getAttribute('data-kind-filter'),
      c.textContent?.replace(/\s+/g, ' ').trim(),
    ]);
    expect(chips).toEqual([
      ['table', 'Tables 6'],
      ['view', 'View 1'],
      ['procedure', 'Procedure 1'],
      ['enum', 'Enum 1'],
    ]);
  });

  it('hides a kind that is switched off, and a schema left with nothing', async () => {
    const { container } = open();
    await fireEvent.click(container.querySelector('[data-kind-filter="table"]')!);
    expect(listed(container).some((e) => e.startsWith('table:'))).toBe(false);
    expect(container.querySelector('[data-schema-group="auth"]')).toBeNull();
    expect(container.querySelector('[data-schema-group="shop"]')).not.toBeNull();
  });

  it('searches within the kinds that are on', async () => {
    const { container } = open();
    await fireEvent.click(container.querySelector('[data-kind-filter="table"]')!);
    await fireEvent.input(container.querySelector('input')!, { target: { value: 'order' } });
    expect(listed(container)).toEqual(['view:shop.order_totals', 'procedure:shop.place_order', 'enum:shop.order_status']);
  });

  it('opens any entity it lists', async () => {
    const { container, onPick } = open();
    await fireEvent.click(container.querySelector('[data-entity-key="shop.order_totals"]')!);
    expect(onPick).toHaveBeenCalledWith('shop.order_totals');
  });
});
