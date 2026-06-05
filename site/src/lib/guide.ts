import { marked } from 'marked';

// Raw markdown is synced from /docs/guide at prebuild. We render it with marked
// (robust against prose punctuation like `version < latest`) and inject as HTML.
const raws = import.meta.glob('./content/guide/*.md', {
	eager: true,
	query: '?raw',
	import: 'default'
});

marked.setOptions({ gfm: true });

// Give headings slug ids so in-page anchors (e.g. #dbml) resolve.
function addHeadingIds(html: string): string {
	return html.replace(/<(h[1-6])>(.*?)<\/\1>/g, (_full, tag, inner) => {
		const text = inner.replace(/<[^>]+>/g, '');
		const id = text
			.toLowerCase()
			.trim()
			.replace(/[^\w]+/g, '-')
			.replace(/^-+|-+$/g, '');
		return `<${tag} id="${id}">${inner}</${tag}>`;
	});
}

// Rewrite inter-doc markdown links like `03-design-yaml.md#dbml` (relative to
// /docs/guide) to the site's route `/guide/design-yaml#dbml`.
function rewriteLinks(html: string): string {
	return html.replace(/href="([^"]+)"/g, (full, href) => {
		const m = href.match(/^(?:\.?\/)?(?:guide\/)?(?:\d+[-_])?([a-z0-9-]+)\.md(#[^"]*)?$/i);
		return m ? `href="/guide/${m[1]}${m[2] ?? ''}"` : full;
	});
}

export type GuidePage = { slug: string; order: number; title: string; html: string };

function titleFrom(raw: string, fallback: string): string {
	const m = raw.match(/^#\s+(.+)$/m);
	return m ? m[1].trim() : fallback;
}

export const pages: GuidePage[] = Object.entries(raws)
	.map(([path, raw]) => {
		const md = raw as string;
		const file = (path.split('/').pop() ?? '').replace('.md', '');
		const order = Number.parseInt(file.slice(0, 2), 10) || 99;
		const slug = file.replace(/^\d+[-_]?/, '');
		return {
			slug,
			order,
			title: titleFrom(md, slug),
			html: rewriteLinks(addHeadingIds(marked.parse(md) as string))
		};
	})
	.sort((a, b) => a.order - b.order);

export function findPage(slug: string): GuidePage | undefined {
	return pages.find((p) => p.slug === slug);
}
