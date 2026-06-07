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
	import { themable } from '@rokkit/actions';
	import { browser } from '$app/environment';
	import Nav from '$lib/components/Nav.svelte';
	import Footer from '$lib/components/Footer.svelte';

	let { children } = $props();

	// Seed vibe from the mode the pre-paint themeHook resolved so first visit
	// (no stored pref) honours the light default rather than vibe's 'dark'.
	// `use:themable` then owns the bridge: it loads from storage, writes
	// data-mode/style/density to this element + <html>, persists on change, and
	// syncs cross-tab — driven by vibe, which the nav's ThemeSwitcherToggle sets.
	if (browser) {
		const dm = document.documentElement.dataset.mode;
		if (dm === 'light' || dm === 'dark') vibe.mode = dm;
	}
</script>

<div use:themable={{ theme: vibe, storageKey: 'dbd-theme' }} class="flex min-h-screen flex-col">
	<Nav />
	<main class="flex-1">
		{@render children()}
	</main>
	<Footer />
</div>

