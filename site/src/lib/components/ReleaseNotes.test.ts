import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ReleaseNotes from './ReleaseNotes.svelte';
import { changelog } from '$lib/data';
import type { Release } from '$lib/releases';

const releases: Release[] = [
	{
		version: '0.3.0',
		date: '2026-10-06',
		url: 'https://github.com/sensei-hq/dbd/releases/tag/v0.3.0',
		html: '<h3>Fixed</h3><ul><li>a data-loss fix</li></ul>'
	},
	{
		version: '0.2.0',
		date: '2026-09-01',
		url: 'https://github.com/sensei-hq/dbd/releases/tag/v0.2.0',
		html: '<h3>Added</h3><ul><li>a new command</li></ul>'
	}
];

it('renders every release with its version, date and notes', () => {
	const { container } = render(ReleaseNotes, { releases });

	const headings = screen.getAllByRole('heading', { level: 2 });
	expect(headings).toHaveLength(releases.length);
	releases.forEach((r, i) => {
		expect(headings[i].textContent).toContain(r.version);
		expect(headings[i].textContent).toContain(r.date);
		expect(container.querySelector(`a[href="${r.url}"]`)).toBeTruthy();
	});
	expect(screen.getByText('a data-loss fix')).toBeTruthy();
	expect(screen.getByText('a new command')).toBeTruthy();
});

it('links to the full changelog on GitHub', () => {
	render(ReleaseNotes, { releases });
	const full = screen.getByRole('link', { name: changelog.fullLabel });
	expect(full.getAttribute('href')).toBe(changelog.full);
	expect(changelog.full).toMatch(/^https:\/\/github\.com\/.+\/CHANGELOG\.md$/);
});

it('still offers the full changelog when no release could be read', () => {
	render(ReleaseNotes, { releases: [] });
	expect(screen.getByRole('link', { name: changelog.fullLabel })).toBeTruthy();
});
