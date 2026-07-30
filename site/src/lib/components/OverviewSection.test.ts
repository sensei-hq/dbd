import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import OverviewSection from './OverviewSection.svelte';
import { overview } from '$lib/data';

it('renders a FeatureCard for every overview feature', () => {
	render(OverviewSection);

	expect(screen.getByText(overview.title)).toBeTruthy();
	const headings = screen.getAllByRole('heading', { level: 3 });
	expect(headings).toHaveLength(overview.features.length);
	for (const f of overview.features) {
		expect(screen.getByText(f.title)).toBeTruthy();
		expect(screen.getByText(f.body)).toBeTruthy();
	}
});
