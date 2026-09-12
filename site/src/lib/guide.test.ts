import { describe, it, expect } from 'vitest';
import { marked } from 'marked';
import { addHeadingIds } from './guide';

marked.setOptions({ gfm: true });

/** The id a heading actually gets, driven through the same marked pipeline the
 *  page build uses — so these assert on real rendered HTML, not a hand-written
 *  approximation of it. */
function idFor(markdown: string): string {
	const html = addHeadingIds(marked.parse(markdown) as string);
	return (html.match(/id="([^"]*)"/) ?? [])[1] ?? '';
}

describe('addHeadingIds', () => {
	it('slugs a plain heading', () => {
		expect(idFor('## Plain heading')).toBe('plain-heading');
	});

	// The guide writes `<type>` / `<schema>` / `<name>` constantly, and marked
	// renders those as &lt;…&gt; inside <code>. Entity references have to be
	// treated as punctuation, or the escape leaks its own name into the anchor.
	it('does not leak HTML entity names into the id', () => {
		expect(idFor('## The `<type>` folder')).toBe('the-type-folder');
	});

	it('treats a bare escaped comparison as punctuation', () => {
		expect(idFor('## Version < 2 and `code`')).toBe('version-2-and-code');
	});

	// Guards the opposite error: tags are markup, not word separators, so
	// stripping them must not split a word that inline emphasis ran through.
	it('does not split a word that inline markup runs through', () => {
		expect(idFor('## mid**dle**')).toBe('middle');
	});

	it('keeps the heading content untouched — only the id is derived', () => {
		const html = addHeadingIds(marked.parse('## The `<type>` folder') as string);
		expect(html).toContain('<code>&lt;type&gt;</code>');
	});
});
