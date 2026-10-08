import { describe, it, expect } from 'vitest';
import real from '../../../CHANGELOG.md?raw';
import { parseReleases } from './releases';

// A Keep-a-Changelog file in the shape the repo's own CHANGELOG.md has: an
// `[Unreleased]` heading first, an em dash between version and date, link
// reference definitions both inside a release and collected at the bottom.
const changelog = `# Changelog

All notable changes are documented here.

## [Unreleased]

### Added

- something not released yet

## [0.3.0] — 2026-10-06

**A lead paragraph.** See [#40].

### Fixed

- a fix ([#40])

[#40]: https://github.com/acme/tool/issues/40

## [0.2.1] — 2026-10-03

### Added

- a feature whose link is defined at the bottom of the file ([#12])

## [0.2.0] - 2026-09-01

### Changed

- a change

## [0.1.0] — 2026-08-01

### Added

- the first feature

## [0.0.9] — 2026-07-01

### Fixed

- an early fix

## [0.0.8] — 2026-06-01

### Fixed

- the sixth release, one past the default count

[#12]: https://github.com/acme/tool/issues/12
[Unreleased]: https://github.com/acme/tool/compare/v0.3.0...main
[0.3.0]: https://github.com/acme/tool/releases/tag/v0.3.0
`;

describe('parseReleases', () => {
	it('returns the newest five releases, newest first', () => {
		expect(parseReleases(changelog).map((r) => r.version)).toEqual([
			'0.3.0',
			'0.2.1',
			'0.2.0',
			'0.1.0',
			'0.0.9'
		]);
	});

	it('skips [Unreleased] and never folds its notes into a release', () => {
		const releases = parseReleases(changelog, 10);
		expect(releases.map((r) => r.version)).not.toContain('Unreleased');
		for (const r of releases) expect(r.html).not.toContain('not released yet');
	});

	it('keeps each release date, whichever dash separates it', () => {
		const byVersion = Object.fromEntries(parseReleases(changelog).map((r) => [r.version, r.date]));
		expect(byVersion['0.3.0']).toBe('2026-10-06');
		expect(byVersion['0.2.0']).toBe('2026-09-01');
	});

	it('renders the section headings inside a release', () => {
		const [latest] = parseReleases(changelog);
		expect(latest.html).toMatch(/<h3[^>]*>Fixed<\/h3>/);
		expect(latest.html).toContain('<strong>A lead paragraph.</strong>');
	});

	it('stops a release at the next release heading', () => {
		const [latest] = parseReleases(changelog);
		expect(latest.html).not.toContain('defined at the bottom');
	});

	// A release's issue links are often defined at the very end of the file, far
	// from the section that uses them. Rendering a slice alone would print the
	// literal `[#12]` instead of a link.
	it('resolves reference links defined anywhere in the file', () => {
		const [latest, second] = parseReleases(changelog);
		expect(latest.html).toContain('href="https://github.com/acme/tool/issues/40"');
		expect(second.html).toContain('href="https://github.com/acme/tool/issues/12"');
	});

	it('links a release to its own definition, or to its GitHub tag', () => {
		const [latest, second] = parseReleases(changelog);
		expect(latest.url).toBe('https://github.com/acme/tool/releases/tag/v0.3.0');
		expect(second.url).toBe('https://github.com/sensei-hq/dbd/releases/tag/v0.2.1');
	});

	it('honours a smaller count and returns everything when there are fewer', () => {
		expect(parseReleases(changelog, 2)).toHaveLength(2);
		expect(parseReleases(changelog, 50)).toHaveLength(6);
		expect(parseReleases('# Changelog\n\n## [Unreleased]\n')).toEqual([]);
	});
});

// The page is fed the repo's real CHANGELOG.md, so the parser must read it — a
// heading format it does not recognise would leave the page empty, silently.
describe('the repository CHANGELOG.md', () => {
	it('yields five dated, semver releases with content', () => {
		const releases = parseReleases(real);
		expect(releases).toHaveLength(5);
		for (const r of releases) {
			expect(r.version).toMatch(/^\d+\.\d+\.\d+$/);
			expect(r.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
			expect(r.html.length).toBeGreaterThan(0);
		}
	});
});
