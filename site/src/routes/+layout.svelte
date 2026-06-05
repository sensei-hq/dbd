<script lang="ts">
	import 'virtual:uno.css';
	import '@fontsource/space-grotesk/400.css';
	import '@fontsource/space-grotesk/500.css';
	import '@fontsource/space-grotesk/600.css';
	import '@fontsource/space-grotesk/700.css';
	import '@fontsource/ibm-plex-sans/400.css';
	import '@fontsource/ibm-plex-sans/500.css';
	import '@fontsource/ibm-plex-sans/600.css';
	import '@fontsource/ibm-plex-mono/400.css';
	import '@fontsource/ibm-plex-mono/500.css';
	import '../app.css';
	import { vibe } from '@rokkit/states';
	import { browser } from '$app/environment';
	import Nav from '$lib/components/Nav.svelte';
	import Footer from '$lib/components/Footer.svelte';

	let { children } = $props();

	// Seed the vibe store from the mode the pre-paint hook already resolved,
	// so the toggle reflects reality with no flash.
	if (browser) {
		const dm = document.documentElement.dataset.mode;
		vibe.mode = dm === 'dark' ? 'dark' : 'light';
	}

	// Keep the DOM + persisted storage in sync with the vibe store.
	$effect(() => {
		const m = vibe.mode;
		document.documentElement.dataset.mode = m;
		document.body.dataset.mode = m;
		vibe.save('dbd-theme');
	});
</script>

<div class="flex min-h-screen flex-col">
	<Nav />
	<main class="flex-1">
		{@render children()}
	</main>
	<Footer />
</div>

