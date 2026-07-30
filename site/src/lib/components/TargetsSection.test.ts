import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import TargetsSection from './TargetsSection.svelte';
import { targets } from '$lib/data';

it('renders a TargetCard for every deployment target', () => {
	render(TargetsSection);

	expect(screen.getByText(targets.title)).toBeTruthy();
	const headings = screen.getAllByRole('heading', { level: 3 });
	expect(headings).toHaveLength(targets.items.length);
	for (const t of targets.items) {
		expect(screen.getByText(t.name)).toBeTruthy();
		expect(screen.getByText(t.scheme)).toBeTruthy();
	}
});
