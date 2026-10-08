<script lang="ts">
	import '$lib/prose.css';
	import { pages, findPage } from '$lib/guide';
	import Seo from '$lib/components/Seo.svelte';

	let { data }: { data: { slug: string } } = $props();

	const current = $derived(findPage(data.slug));
	const idx = $derived(pages.findIndex((p) => p.slug === data.slug));
	const prev = $derived(idx > 0 ? pages[idx - 1] : undefined);
	const next = $derived(idx >= 0 && idx < pages.length - 1 ? pages[idx + 1] : undefined);
</script>

<Seo
	title="{current?.title ?? 'Guide'} — dbd"
	description={current?.description ?? 'dbd documentation: managing database schemas as code.'}
	type="article"
/>

<div class="mx-auto grid max-w-content gap-12 px-6 py-section lg:grid-cols-[16rem_1fr]">
	<!-- Sidebar -->
	<aside class="lg:sticky lg:top-24 lg:self-start">
		<span class="font-mono text-label uppercase text-ink-soft">Guide</span>
		<nav class="mt-4 flex flex-col gap-1">
			{#each pages as p (p.slug)}
				<a
					href="/guide/{p.slug}"
					aria-current={p.slug === data.slug ? 'page' : undefined}
					class="rounded-md px-3 py-2 text-sm transition-colors {p.slug === data.slug
						? 'bg-accent-soft text-primary font-medium'
						: 'text-ink-mute hover:bg-paper-soft hover:text-ink'}"
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
			<div class="guide-prose text-ink-mute">{@html current.html}</div>
		{/if}

		<div class="mt-16 flex items-center justify-between gap-4 border-t border-paper-edge pt-6">
			{#if prev}
				<a href="/guide/{prev.slug}" class="text-sm text-ink-mute hover:text-primary">← {prev.title}</a>
			{:else}
				<span></span>
			{/if}
			{#if next}
				<a href="/guide/{next.slug}" class="text-sm text-ink-mute hover:text-primary">{next.title} →</a>
			{/if}
		</div>
	</article>
</div>

