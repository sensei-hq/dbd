import adapter from '@sveltejs/adapter-auto';
import { mdsvex } from 'mdsvex';

const mdsvexConfig = {
	extensions: ['.md']
};

/** @type {import('@sveltejs/kit').Config} */
const config = {
	extensions: ['.svelte', '.md'],
	preprocess: [mdsvex(mdsvexConfig)],
	compilerOptions: {
		// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
		runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
	},
	kit: {
		// adapter-auto detects Vercel at build time. Every route is prerendered
		// (see src/routes/+layout.ts), so this ships as static output.
		adapter: adapter()
	}
};

export default config;
