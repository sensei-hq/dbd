import { describe, it, expect } from 'vitest';
import reference from '../../../docs/guide/04-commands.md?raw';
import { nav, footer, hero, overview, concepts, commands, start } from './data';

const hrefs = (links: { href: string }[]) => links.map((l) => l.href);
const footerHrefs = () => footer.columns.flatMap((c) => hrefs(c.links));

describe('site navigation', () => {
	it('links the schema viewer and the changelog from the header', () => {
		expect(hrefs(nav.links)).toContain('/diagram');
		expect(hrefs(nav.links)).toContain('/changelog');
	});

	it('links the viewer, the changelog and both llms files from the footer', () => {
		for (const href of ['/diagram', '/changelog', '/llms.txt', '/llms-full.txt']) {
			expect(footerHrefs()).toContain(href);
		}
	});
});

// The home page is where most people decide what dbd can do. A command the CLI
// reference documents but the page never names reads as a missing feature, so
// the page's command inventory is checked against the reference itself rather
// than against a hand-kept list that would drift the same way the page did.
describe('the home page', () => {
	const documented = [...reference.matchAll(/^## `dbd ([a-z-]+)/gm)].map((m) => m[1]);

	const shown = [
		...hero.terminal.lines.map((l) => l.text),
		...overview.features.map((f) => f.body),
		...concepts.items.flatMap((c) => [c.body, c.code.source]),
		...commands.items.map((c) => c.cmd),
		...start.steps.map((s) => s.code)
	].join('\n');

	it('reads the command list from the reference', () => {
		expect(documented.length).toBeGreaterThan(15);
	});

	/** `dbd <cmd>` as a whole word: `dbd init --from-db` names `init`, but
	 *  `dbd reconcile` must not count as naming a command called `re`. */
	function names(cmd: string): boolean {
		const needle = `dbd ${cmd}`;
		for (let i = shown.indexOf(needle); i !== -1; i = shown.indexOf(needle, i + 1)) {
			const before = shown[i - 1] ?? ' ';
			const after = shown[i + needle.length] ?? ' ';
			if (!/[\w-]/.test(before) && !/[\w-]/.test(after)) return true;
		}
		return false;
	}

	it('names every command the commands reference documents', () => {
		expect(documented.filter((cmd) => !names(cmd))).toEqual([]);
	});
});
