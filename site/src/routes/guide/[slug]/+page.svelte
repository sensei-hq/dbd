<script lang="ts">
	import { pages, findPage } from '$lib/guide';

	let { data }: { data: { slug: string } } = $props();

	const current = $derived(findPage(data.slug));
	const idx = $derived(pages.findIndex((p) => p.slug === data.slug));
	const prev = $derived(idx > 0 ? pages[idx - 1] : undefined);
	const next = $derived(idx >= 0 && idx < pages.length - 1 ? pages[idx + 1] : undefined);
</script>

<svelte:head>
	<title>{current?.title ?? 'Guide'} — dbd</title>
</svelte:head>

<div class="mx-auto grid max-w-content gap-12 px-6 py-section lg:grid-cols-[16rem_1fr]">
	<!-- Sidebar -->
	<aside class="lg:sticky lg:top-24 lg:self-start">
		<span class="font-mono text-label uppercase text-surface-z5">Guide</span>
		<nav class="mt-4 flex flex-col gap-1">
			{#each pages as p (p.slug)}
				<a
					href="/guide/{p.slug}"
					aria-current={p.slug === data.slug ? 'page' : undefined}
					class="rounded-md px-3 py-2 text-sm transition-colors {p.slug === data.slug
						? 'bg-primary-z1 text-primary-z6 font-medium'
						: 'text-surface-z7 hover:bg-surface-z1 hover:text-surface-z9'}"
				>
					{p.title}
				</a>
			{/each}
		</nav>
	</aside>

	<!-- Content -->
	<article class="min-w-0">
		{#if current}
			<!-- eslint-disable-next-line svelte/no-at-html-tags — trusted, build-time docs from /docs/guide -->
			<div class="guide-prose text-surface-z8">{@html current.html}</div>
		{/if}

		<div class="mt-16 flex items-center justify-between gap-4 border-t border-surface-z2 pt-6">
			{#if prev}
				<a href="/guide/{prev.slug}" class="text-sm text-surface-z7 hover:text-primary-z5">← {prev.title}</a>
			{:else}
				<span></span>
			{/if}
			{#if next}
				<a href="/guide/{next.slug}" class="text-sm text-surface-z7 hover:text-primary-z5">{next.title} →</a>
			{/if}
		</div>
	</article>
</div>

<style>
	.guide-prose {
		line-height: 1.7;
		font-size: 0.975rem;
	}
	.guide-prose :global(h1) {
		font-family: '"Space Grotesk"', 'Space Grotesk', system-ui, sans-serif;
		font-size: 2.2rem;
		font-weight: 700;
		letter-spacing: -0.02em;
		line-height: 1.1;
		color: inherit;
		margin: 0 0 1.25rem;
	}
	.guide-prose :global(h2) {
		font-family: '"Space Grotesk"', 'Space Grotesk', system-ui, sans-serif;
		font-size: 1.5rem;
		font-weight: 600;
		letter-spacing: -0.015em;
		color: inherit;
		margin: 2.75rem 0 1rem;
	}
	.guide-prose :global(h3) {
		font-family: '"Space Grotesk"', 'Space Grotesk', system-ui, sans-serif;
		font-size: 1.2rem;
		font-weight: 600;
		color: inherit;
		margin: 2rem 0 0.75rem;
	}
	.guide-prose :global(p),
	.guide-prose :global(ul),
	.guide-prose :global(ol),
	.guide-prose :global(table) {
		margin: 0 0 1.1rem;
	}
	.guide-prose :global(ul),
	.guide-prose :global(ol) {
		padding-left: 1.4rem;
	}
	.guide-prose :global(li) {
		margin: 0.35rem 0;
	}
	.guide-prose :global(a) {
		color: oklch(0.55 0.13 245);
		text-decoration: none;
	}
	.guide-prose :global(a:hover) {
		text-decoration: underline;
	}
	:global([data-mode='dark']) .guide-prose :global(a) {
		color: oklch(0.78 0.12 245);
	}
	.guide-prose :global(strong) {
		font-weight: 600;
		color: inherit;
	}
	.guide-prose :global(code) {
		font-family: '"IBM Plex Mono"', 'IBM Plex Mono', ui-monospace, monospace;
		font-size: 0.85em;
		padding: 0.12rem 0.35rem;
		border-radius: 0.35rem;
		background: rgba(127, 127, 127, 0.12);
	}
	.guide-prose :global(pre) {
		font-family: '"IBM Plex Mono"', 'IBM Plex Mono', ui-monospace, monospace;
		font-size: 0.82rem;
		line-height: 1.6;
		padding: 1.1rem 1.25rem;
		border-radius: 0.85rem;
		overflow-x: auto;
		background: rgba(127, 127, 127, 0.10);
		border: 1px solid rgba(127, 127, 127, 0.2);
		margin: 0 0 1.4rem;
	}
	.guide-prose :global(pre code) {
		padding: 0;
		background: none;
	}
	.guide-prose :global(table) {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9rem;
	}
	.guide-prose :global(th),
	.guide-prose :global(td) {
		text-align: left;
		padding: 0.5rem 0.75rem;
		border: 1px solid rgba(127, 127, 127, 0.2);
	}
	.guide-prose :global(blockquote) {
		border-left: 3px solid oklch(0.55 0.13 245 / 0.5);
		padding-left: 1rem;
		margin: 0 0 1.1rem;
		opacity: 0.85;
	}
</style>
