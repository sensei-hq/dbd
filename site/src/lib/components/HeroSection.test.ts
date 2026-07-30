import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import HeroSection from './HeroSection.svelte';
import { hero } from '$lib/data';

it('renders the hero title, eyebrow, both CTAs and the install command', () => {
	const { container } = render(HeroSection);

	expect(screen.getByText(hero.eyebrow)).toBeTruthy();
	expect(container.textContent).toContain(hero.title[0]);
	expect(container.textContent).toContain(hero.title[1]);
	expect(screen.getByText(hero.lede)).toBeTruthy();

	const primary = screen.getByRole('link', { name: new RegExp(hero.primaryCta.label) });
	expect(primary.getAttribute('href')).toBe(hero.primaryCta.href);
	const secondary = screen.getByRole('link', { name: hero.secondaryCta.label });
	expect(secondary.getAttribute('href')).toBe(hero.secondaryCta.href);

	expect(screen.getByText(hero.install)).toBeTruthy();
});
