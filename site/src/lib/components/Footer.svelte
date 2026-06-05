<script lang="ts">
	import BrandMark from './BrandMark.svelte';
	import { brand, footer } from '$lib/data';
</script>

<footer class="bg-surface-z0">
	<div class="mx-auto grid max-w-content gap-10 px-6 py-14 md:grid-cols-[1.4fr_1fr_1fr]">
		<div class="flex flex-col gap-3">
			<a href="/" class="inline-flex items-center gap-2.5">
				<BrandMark class="h-8 w-8" />
				<span class="font-display font-semibold text-lg tracking-tight text-surface-z9">{brand.name}</span>
			</a>
			<p class="max-w-xs text-sm text-surface-z7">{footer.tagline}</p>
		</div>
		{#each footer.columns as col (col.title)}
			<div class="flex flex-col gap-3">
				<span class="font-mono text-label uppercase text-surface-z5">{col.title}</span>
				<ul class="flex flex-col gap-2">
					{#each col.links as l (l.label)}
						<li>
							<a href={l.href} class="text-sm text-surface-z7 transition-colors hover:text-surface-z9"
								>{l.label}</a
							>
						</li>
					{/each}
				</ul>
			</div>
		{/each}
	</div>
	<div class="border-t border-surface-z2">
		<div
			class="mx-auto flex max-w-content flex-col gap-2 px-6 py-5 sm:flex-row sm:items-center sm:justify-between"
		>
			<span class="font-mono text-xs text-surface-z5">{brand.name} — {brand.full}</span>
			<span class="text-xs text-surface-z5">{footer.legal}</span>
		</div>
	</div>
</footer>
