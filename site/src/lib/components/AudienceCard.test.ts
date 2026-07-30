import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import AudienceCard from './AudienceCard.svelte';

it('renders the title and body props', () => {
	render(AudienceCard, { title: 'DevOps & platform teams', body: 'Automating database deployments straight from Git.' });
	expect(screen.getByText('DevOps & platform teams')).toBeTruthy();
	expect(screen.getByText('Automating database deployments straight from Git.')).toBeTruthy();
});
