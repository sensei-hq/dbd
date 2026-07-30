<script lang="ts">
	import { Button } from '@rokkit/ui';
	import SectionHead from '$lib/components/SectionHead.svelte';
	import ArrowIcon from '$lib/components/ArrowIcon.svelte';
	import StartStep from '$lib/components/StartStep.svelte';
	import { start } from '$lib/data';
</script>

<!-- Get started -->
<section id="start">
	<div class="mx-auto max-w-content px-6 py-section">
		<div class="overflow-hidden rounded-xl border border-accent bg-paper-mute">
			<div class="grid gap-10 p-8 lg:grid-cols-[1fr_1.1fr] lg:p-12">
				<div class="flex flex-col gap-4">
					<SectionHead eyebrow={start.eyebrow} title={start.title} lede={start.lede} />
					<div class="pt-2">
						<Button href={start.cta.href} variant="primary" size="lg">
							{start.cta.label}
							<ArrowIcon />
						</Button>
					</div>
				</div>
				<div class="flex flex-col gap-3">
					{#each start.steps as s (s.n)}
						<StartStep n={s.n} text={s.text} code={s.code} />
					{/each}
				</div>
			</div>
		</div>
	</div>
</section>
