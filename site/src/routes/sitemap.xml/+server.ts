import { SITE_URL, indexableRoutes } from '$lib/seo';
import { pages } from '$lib/guide';

export const prerender = true;

export function GET() {
	const routes = indexableRoutes(pages.map((p) => p.slug));
	const urls = routes
		.map((r) => `\t<url><loc>${SITE_URL}${r === '/' ? '/' : r}</loc></url>`)
		.join('\n');
	const xml = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls}
</urlset>
`;
	return new Response(xml, {
		headers: { 'content-type': 'application/xml; charset=utf-8' }
	});
}
