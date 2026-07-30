import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import CommandsSection from './CommandsSection.svelte';
import { commands } from '$lib/data';

it('renders a CommandCard for every command', () => {
	const { container } = render(CommandsSection);

	expect(screen.getByText(commands.title)).toBeTruthy();
	expect(container.querySelectorAll('code').length).toBe(commands.items.length);
	for (const c of commands.items) {
		expect(screen.getByText(c.cmd)).toBeTruthy();
		expect(screen.getByText(c.body)).toBeTruthy();
	}
});
