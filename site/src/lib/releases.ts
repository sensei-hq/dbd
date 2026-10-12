import { marked } from 'marked';

/** One released version from CHANGELOG.md, rendered for the changelog page. */
export type Release = {
	version: string;
	/** `YYYY-MM-DD`, or null when the heading carries no date. */
	date: string | null;
	/** The release's own link: its reference definition, else its GitHub tag. */
	url: string;
	/** The release's notes (lead paragraph and `###` sections) as HTML. */
	html: string;
};

const TAG_URL = 'https://github.com/sensei-hq/dbd/releases/tag/v';

/** `## [0.24.1] — 2026-10-06`. Keep a Changelog writes a dash between version and
 *  date; the repo uses an em dash, other tools a hyphen or an en dash. */
const RELEASE_HEADING = /^## \[([^\]]+)\](?:\s*[—–-]\s*(\d{4}-\d{2}-\d{2}))?/;

/** `[#40]: https://…` — a link reference definition, on a line of its own. */
const DEFINITION = /^\[([^\]]+)\]:\s+(\S+)/;

/**
 * The newest `count` releases in a Keep a Changelog document, newest first.
 *
 * `[Unreleased]` is skipped — its notes describe work no version ships yet.
 * Link reference definitions are collected from the whole file and given to
 * every release, because a changelog defines most of them once, at the bottom,
 * far from the section that uses them; a slice rendered alone would print the
 * literal `[#12]`.
 */
export function parseReleases(markdown: string, count = 5): Release[] {
	const lines = markdown.split('\n');

	const definitions = new Map<string, string>();
	for (const line of lines) {
		const m = line.match(DEFINITION);
		if (m) definitions.set(m[1].toLowerCase(), m[2]);
	}
	const references = [...definitions].map(([label, url]) => `[${label}]: ${url}`).join('\n');

	type Section = { version: string; date: string | null; body: string[] };
	const sections: Section[] = [];
	let current: Section | null = null;
	for (const line of lines) {
		const heading = line.match(RELEASE_HEADING);
		if (heading) {
			current = { version: heading[1].trim(), date: heading[2] ?? null, body: [] };
			sections.push(current);
		} else if (line.startsWith('## ') || line.startsWith('# ')) {
			current = null; // a heading that is not a release ends the one before it
		} else if (current && !DEFINITION.test(line)) {
			current.body.push(line);
		}
	}

	return sections
		.filter((s) => s.version.toLowerCase() !== 'unreleased')
		.slice(0, count)
		.map((s) => ({
			version: s.version,
			date: s.date,
			url: definitions.get(s.version.toLowerCase()) ?? `${TAG_URL}${s.version}`,
			html: marked.parse(`${s.body.join('\n').trim()}\n\n${references}\n`, {
				gfm: true,
				async: false
			}) as string
		}));
}
