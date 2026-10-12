import { it, expect } from 'vitest';
import { indexableRoutes } from './seo';

it('lists every public page for the sitemap, guide pages included', () => {
	const routes = indexableRoutes(['what-is-dbd', 'commands']);
	for (const r of ['/', '/guide', '/diagram', '/changelog', '/guide/what-is-dbd', '/guide/commands']) {
		expect(routes).toContain(r);
	}
});

// /projects is a personal, localStorage-backed workspace: nothing to crawl.
it('keeps /projects out of the sitemap', () => {
	expect(indexableRoutes([])).not.toContain('/projects');
});
