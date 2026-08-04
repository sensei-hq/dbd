/**
 * Prebuild step: pull docs from the repo into the site.
 *
 *  - docs/llms/*.txt          → src/lib/content/llms/  (served by +server.ts routes
 *                                with an explicit text/plain; charset=utf-8 header)
 *  - docs/guide/*.md          → src/lib/content/guide/ (rendered at /guide/<slug>)
 *
 * Keeps a single source of truth in docs/ — the site never forks the content.
 */
import { mkdirSync, copyFileSync, readdirSync, existsSync, rmSync, cpSync } from 'node:fs';
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

// 1. llms files → src/lib/content/llms/ (served via routes with charset=utf-8)
const llmsDest = join(here, '..', 'src', 'lib', 'content', 'llms');
mkdirSync(llmsDest, { recursive: true });
for (const name of ['llms.txt', 'llms-full.txt']) {
	const src = join(docs, 'llms', name);
	if (existsSync(src)) copyInto(src, join(llmsDest, name));
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

// 3. skills + agents + manifest → static/ (served as raw files at the site root,
//    so the library is installable via sensei.library.json from the site and GitHub).
const staticDir = join(here, '..', 'static');
const skillsSrc = join(docs, 'skills');
const agentsSrc = join(docs, 'agents');
if (existsSync(skillsSrc)) {
	rmSync(join(staticDir, 'skills'), { recursive: true, force: true });
	cpSync(skillsSrc, join(staticDir, 'skills'), { recursive: true });
	console.log(`  copied docs/skills → ${join(staticDir, 'skills').replace(repo + '/', '')}`);
}
if (existsSync(agentsSrc)) {
	rmSync(join(staticDir, 'agents'), { recursive: true, force: true });
	cpSync(agentsSrc, join(staticDir, 'agents'), { recursive: true });
	console.log(`  copied docs/agents → ${join(staticDir, 'agents').replace(repo + '/', '')}`);
}
const manifest = join(repo, 'sensei.library.json');
if (existsSync(manifest)) {
	copyInto(manifest, join(staticDir, 'sensei.library.json'));
	copyInto(manifest, join(staticDir, '.well-known', 'sensei.library.json'));
}

console.log('content sync complete.');
