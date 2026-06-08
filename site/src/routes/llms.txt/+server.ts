import content from '$lib/content/llms/llms.txt?raw';

export const prerender = true;

// Serve as UTF-8 plain text. Static .txt hosting (and the vite dev server) can
// omit the charset, making the em-dashes render as mojibake (â€”) — set it
// explicitly here.
export function GET() {
	return new Response(content, {
		headers: { 'content-type': 'text/plain; charset=utf-8' }
	});
}
