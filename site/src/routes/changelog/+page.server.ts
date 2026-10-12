import { parseReleases } from '$lib/releases';

export const prerender = true;

// A server load, so only the rendered releases reach the browser — not the whole
// file. CHANGELOG.md is copied from the repo root into the gitignored content dir at
// prebuild/predev (scripts/copy-content.mjs), the same way the guide is.
const copies = import.meta.glob('../../lib/content/CHANGELOG.md', {
	eager: true,
	query: '?raw',
	import: 'default'
});

export function load() {
	const markdown = Object.values(copies)[0] as string | undefined;
	// Fail the build rather than ship an empty page: no copy means the prebuild
	// step did not run, and no releases means the heading format changed.
	if (!markdown) throw new Error('CHANGELOG.md was not copied into src/lib/content — run `bun run sync:content`');
	const releases = parseReleases(markdown, 5);
	if (releases.length === 0) throw new Error('CHANGELOG.md has no release headings this page can read');
	return { releases };
}
