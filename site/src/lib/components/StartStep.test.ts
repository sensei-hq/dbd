import { it, expect } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StartStep from './StartStep.svelte';

it('renders the step number, text and code props', () => {
	const { container } = render(StartStep, { n: '2', text: 'Scaffold a project', code: 'dbd init --name my-project' });
	expect(screen.getByText('2')).toBeTruthy();
	expect(screen.getByText('Scaffold a project')).toBeTruthy();
	expect(container.querySelector('code')?.textContent).toContain('dbd init --name my-project');
});
