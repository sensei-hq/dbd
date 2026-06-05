/* Minimal, dependency-free line tokenizer for the code blocks. Maps tokens to
   Rokkit z-utility colour classes — comments muted, strings success, prompt/arrow accent. */

export type Tok = { t: string; cls: string };

const COMMENT = 'text-surface-z5';
const STR = 'text-success-z6';
const ACCENT = 'text-primary-z5';
const DEF = 'text-surface-z8';

export function highlight(source: string): Tok[][] {
	return source.split('\n').map(tokenizeLine);
}

function tokenizeLine(line: string): Tok[] {
	const trimmed = line.trimStart();
	if (trimmed.startsWith('#') || trimmed.startsWith('--') || trimmed.startsWith('//')) {
		return [{ t: line, cls: COMMENT }];
	}

	const toks: Tok[] = [];
	let last = 0;
	for (const m of line.matchAll(/("[^"]*"|'[^']*'|→|\$)/g)) {
		const idx = m.index ?? 0;
		if (idx > last) toks.push({ t: line.slice(last, idx), cls: DEF });
		const v = m[0];
		toks.push({ t: v, cls: v === '→' || v === '$' ? ACCENT : STR });
		last = idx + v.length;
	}
	if (last < line.length) toks.push({ t: line.slice(last), cls: DEF });
	if (toks.length === 0) toks.push({ t: '', cls: DEF });
	return toks;
}
