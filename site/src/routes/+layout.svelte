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
	import { page } from '$app/state';
	import Nav from '$lib/components/Nav.svelte';
	import Footer from '$lib/components/Footer.svelte';

	let { children } = $props();

	// The /diagram + /projects routes are the full-bleed app shell; hide the
	// marketing-site Nav/Footer there (the design app owns the viewport).
	const isApp = $derived(
		page.url.pathname.startsWith('/diagram') || page.url.pathname.startsWith('/projects')
	);

	// Seed vibe from the mode the pre-paint themeHook resolved so first visit
	// (no stored pref) honours the light default rather than vibe's 'dark'.
	// `use:themable` then owns the bridge: it loads from storage, writes
	// data-mode/style/density to this element + <html>, persists on change, and
	// syncs cross-tab — driven by vibe, which the nav's ThemeSwitcherToggle sets.
	if (browser) {
		// This app ships only the zen-sumi style (app.css imports zen-sumi.css).
		// vibe defaults its style to 'rokkit' and 'zen-sumi' isn't in its default
		// allowed list, so without this `themable` would write data-style="rokkit"
		// (and stale localStorage would reinforce it) — leaving every
		// [data-style="zen-sumi"] component rule inert. Lock vibe to zen-sumi so
		// the style is applied and storage can't override it back.
		vibe.allowedStyles = ['zen-sumi'];
		vibe.style = 'zen-sumi';

		const dm = document.documentElement.dataset.mode;
		if (dm === 'light' || dm === 'dark') vibe.mode = dm;
	}
</script>

<!--
	`themable` must own <body> (not a wrapper div): it writes data-mode/style/density
	to the element it's on and mirrors to <html>. On a div, <body> keeps the stale
	data-mode the pre-paint script set, and since <body> is an ancestor of all content
	its [data-mode] wins — so toggling mode never flips the page. Putting it on
	<svelte:body> keeps body + html in sync (this is what the Rokkit learn app does).
-->
<svelte:body use:themable={{ theme: vibe, storageKey: 'dbd-theme' }} />

<div class="flex min-h-screen flex-col">
	{#if !isApp}<Nav />{/if}
	<main class="flex-1">
		{@render children()}
	</main>
	{#if !isApp}<Footer />{/if}
</div>

