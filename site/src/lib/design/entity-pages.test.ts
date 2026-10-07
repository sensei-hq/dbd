import { it, expect, describe } from 'vitest';
import { render, fireEvent, findByRole } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import ObjectView from './ObjectView.svelte';
import type { SchemaModel } from './model';
import { sampleModel } from './data';

// Every entity the sidebar lists opens a page (#34): a view or routine shows what it uses and
// what uses it, an enum its values and the columns of that type.

const tick = () => new Promise((r) => setTimeout(r, 0));
const text = (el: Element | null | undefined) => el?.textContent?.replace(/\s+/g, ' ').trim();

async function openEntity(key: string) {
  const view = render(Page);
  await tick();
  await fireEvent.click(view.container.querySelector(`aside [data-entity-key="${key}"]`)!);
  await tick();
  return view.container;
}

describe('a view or routine page', () => {
  it('names the entity and its kind', async () => {
    const page = await openEntity('shop.order_totals');
    expect(page.querySelector('[data-object-view]')).not.toBeNull();
    expect(text(page.querySelector('h1'))).toBe('order_totals');
    expect(text(page.querySelector('[data-kind-badge]'))).toBe('view');
  });

  it('lists what it uses and what uses it, from the dependency graph', async () => {
    const page = await openEntity('shop.place_order');
    const rows = (sel: string) => [...page.querySelectorAll(`${sel} [data-dep]`)].map((d) => d.getAttribute('data-dep'));
    const uses = sampleModel.deps!.filter((d) => d.from.s === 'shop' && d.from.n === 'place_order').map((d) => `${d.to.s}.${d.to.n}`);
    expect(rows('[data-section="uses"]')).toEqual(uses);
    expect(page.querySelector('[data-section="used-by"]')?.textContent).toMatch(/nothing/i);
  });

  it('opens a table it uses', async () => {
    const page = await openEntity('shop.order_totals');
    await fireEvent.click(page.querySelector('[data-section="uses"] [data-dep="shop.orders"] button')!);
    await tick();
    expect(text(page.querySelector('h1'))).toBe('orders');
  });

  it('draws its neighbourhood over the dependency graph', async () => {
    const page = await openEntity('shop.order_totals');
    await fireEvent.click(await findByRole(page, 'button', { name: 'Diagram' }));
    await tick();
    expect(page.querySelector('[data-graph-layout]')?.getAttribute('data-graph-layout')).toBe('neighborhood');
    expect(page.querySelector('[data-graph-node="shop.order_totals"]')).not.toBeNull();
    expect(page.querySelector('[data-graph-node="shop.orders"]')).not.toBeNull();
  });

  it('says why it has no changelog', async () => {
    const page = await openEntity('shop.order_totals');
    await fireEvent.click(await findByRole(page, 'button', { name: 'Changelog' }));
    expect(text(page.querySelector('[data-entity-changelog]'))).toMatch(/tables and enums/i);
  });
});

describe('an enum page', () => {
  it('lists its values in order', async () => {
    const page = await openEntity('shop.order_status');
    expect(page.querySelector('[data-enum-view]')).not.toBeNull();
    const values = [...page.querySelectorAll('[data-section="values"] [data-value]')].map((v) => v.getAttribute('data-value'));
    expect(values).toEqual(sampleModel.enums![0].values);
  });

  it('lists the columns of its type, each opening its table', async () => {
    const page = await openEntity('shop.order_status');
    const users = [...page.querySelectorAll('[data-section="used-by"] [data-column]')].map((c) => c.getAttribute('data-column'));
    expect(users).toEqual(['shop.orders.status']);
    await fireEvent.click(page.querySelector('[data-section="used-by"] [data-column="shop.orders.status"] button')!);
    await tick();
    expect(text(page.querySelector('h1'))).toBe('orders');
  });

  it('has a changelog, because snapshots record enums', async () => {
    const page = await openEntity('shop.order_status');
    await fireEvent.click(await findByRole(page, 'button', { name: 'Changelog' }));
    const versions = [...page.querySelectorAll('[data-entity-changelog] [data-version]')].map((v) => v.getAttribute('data-version'));
    expect(versions).toEqual(['5']);
  });
});

describe('a sequence page', () => {
  const withSequence: SchemaModel = {
    ...sampleModel,
    entities: [
      ...(sampleModel.entities ?? []),
      { schema: 'shop', name: 'invoice_no', kind: 'sequence', noteMd: 'Invoice numbers.' },
    ],
  };
  const openSequence = () =>
    render(ObjectView, { props: { model: withSequence, entityKey: 'shop.invoice_no', onNav: () => {} } }).container;

  it('names the sequence and its kind, and describes it by its comment', () => {
    const page = openSequence();
    expect(text(page.querySelector('h1'))).toBe('invoice_no');
    expect(text(page.querySelector('[data-kind-badge]'))).toBe('sequence');
    expect(text(page.querySelector('[data-section="info"]'))).toBe('Invoice numbers.');
  });

  // A column default's `nextval('…')` is recorded as a call to `nextval`, not as a use of the
  // sequence, so "nothing uses it" would be a claim the model cannot back.
  it('does not claim that nothing uses it', () => {
    const usedBy = text(openSequence().querySelector('[data-section="used-by"]'));
    expect(usedBy).not.toMatch(/nothing in this project uses it/i);
    expect(usedBy).toMatch(/nextval/);
  });
});
