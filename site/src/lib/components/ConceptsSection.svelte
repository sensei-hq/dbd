<script lang="ts">
	import SectionHead from '$lib/components/SectionHead.svelte';
	import CodeBlock from '$lib/components/CodeBlock.svelte';
	import { concepts } from '$lib/data';
</script>

<!-- Concepts -->
<section id="concepts">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={concepts.eyebrow} title={concepts.title} align="center" />
		<div class="mt-14 flex flex-col gap-16">
			{#each concepts.items as item, i (item.id)}
				<div class="grid items-center gap-8 lg:grid-cols-2 lg:gap-14">
					<div class="flex flex-col gap-4 {i % 2 === 1 ? 'lg:order-2' : ''}">
						<span class="font-mono text-label uppercase text-ink-soft">{item.kicker}</span>
						<h3 class="font-display font-semibold text-h3 text-ink text-balance">{item.title}</h3>
						<p class="text-base text-ink-mute text-pretty">{item.body}</p>
					</div>
					<div class={i % 2 === 1 ? 'lg:order-1' : ''}>
						<CodeBlock code={item.code} />
					</div>
				</div>
			{/each}
		</div>
	</div>
</section>
