/**
 * Prebuild step: pull docs from the repo into the site.
 *
 *  - docs/llms/llms.txt       → static/llms.txt        (served at /llms.txt)
 *  - docs/llms/llms-full.txt  → static/llms-full.txt   (served at /llms-full.txt)
 *  - docs/guide/*.md          → src/lib/content/guide/ (rendered at /guide/<slug>)
 *
 * Keeps a single source of truth in docs/ — the site never forks the content.
 */
import { mkdirSync, copyFileSync, readdirSync, existsSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..', '..');
const docs = join(repo, 'docs');

function copyInto(srcFile, destFile) {
	mkdirSync(dirname(destFile), { recursive: true });
	copyFileSync(srcFile, destFile);
	console.log(`  copied ${srcFile.replace(repo + '/', '')} → ${destFile.replace(repo + '/', '')}`);
}

// 1. llms files → static/
const staticDir = join(here, '..', 'static');
for (const name of ['llms.txt', 'llms-full.txt']) {
	const src = join(docs, 'llms', name);
	if (existsSync(src)) copyInto(src, join(staticDir, name));
	else console.warn(`  ! missing ${src}`);
}

// 2. guide markdown → src/lib/content/guide/ (cleaned + re-synced each build)
const guideSrc = join(docs, 'guide');
const guideDest = join(here, '..', 'src', 'lib', 'content', 'guide');
rmSync(guideDest, { recursive: true, force: true });
mkdirSync(guideDest, { recursive: true });
if (existsSync(guideSrc)) {
	for (const f of readdirSync(guideSrc).filter((n) => n.endsWith('.md'))) {
		copyInto(join(guideSrc, f), join(guideDest, f));
	}
} else {
	console.warn(`  ! missing ${guideSrc}`);
}

console.log('content sync complete.');
