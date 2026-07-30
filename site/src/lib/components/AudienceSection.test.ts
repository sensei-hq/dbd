import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import AudienceSection from './AudienceSection.svelte';
import { audience } from '$lib/data';

it('renders an AudienceCard for every audience item', () => {
	render(AudienceSection);

	expect(screen.getByText(audience.title)).toBeTruthy();
	const headings = screen.getAllByRole('heading', { level: 3 });
	expect(headings).toHaveLength(audience.items.length);
	for (const a of audience.items) {
		expect(screen.getByText(a.title)).toBeTruthy();
		expect(screen.getByText(a.body)).toBeTruthy();
	}
});
