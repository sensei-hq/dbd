import { it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import Page from '../../routes/diagram/+page.svelte';
import { encodeFragment } from './fragment';
import { SAMPLE_MODEL } from './sample';

it('renders the example when "Load example" is clicked', async () => {
  const { getByText, container } = render(Page);
  await fireEvent.click(getByText('Load example'));
  await new Promise((r) => setTimeout(r, 0));
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
});

it('renders a model decoded from the URL fragment', async () => {
  const frag = await encodeFragment(SAMPLE_MODEL);
  window.location.hash = '#' + frag;
  const { container, findAllByText } = render(Page);
  await findAllByText('customers');
  expect(container.querySelectorAll('[data-card]').length).toBeGreaterThanOrEqual(2);
  window.location.hash = '';
});
