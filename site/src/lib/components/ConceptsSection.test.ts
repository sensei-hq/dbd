import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ConceptsSection from './ConceptsSection.svelte';
import { concepts } from '$lib/data';

it('renders every concept title and its code block', () => {
	const { container } = render(ConceptsSection);

	for (const item of concepts.items) {
		expect(screen.getByText(item.title)).toBeTruthy();
		expect(screen.getByText(item.kicker)).toBeTruthy();
		// The code block header shows the label; its body is syntax-highlighted
		// token-by-token, so check the label as the code-block's identity.
		expect(screen.getByText(item.code.label)).toBeTruthy();
	}
	// Sanity: five concepts, five code blocks.
	expect(container.querySelectorAll('pre').length).toBe(concepts.items.length);
});
