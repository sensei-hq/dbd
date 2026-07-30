import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StartSection from './StartSection.svelte';
import { start } from '$lib/data';

it('renders every start step and the get-started CTA', () => {
	render(StartSection);

	expect(screen.getByText(start.title)).toBeTruthy();
	for (const s of start.steps) {
		expect(screen.getByText(s.text)).toBeTruthy();
		expect(screen.getByText(s.code)).toBeTruthy();
	}

	const cta = screen.getByRole('link', { name: start.cta.label });
	expect(cta.getAttribute('href')).toBe(start.cta.href);
});
