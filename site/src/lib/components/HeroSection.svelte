<script lang="ts">
	import { Button } from '@rokkit/ui';
	import Eyebrow from '$lib/components/Eyebrow.svelte';
	import Terminal from '$lib/components/Terminal.svelte';
	import ArrowIcon from '$lib/components/ArrowIcon.svelte';
	import { hero } from '$lib/data';
</script>

<!-- Hero -->
<section id="top" class="relative overflow-hidden">
	<div
		class="relative mx-auto grid max-w-content items-center gap-12 px-6 pb-section pt-16 lg:grid-cols-[1.05fr_0.95fr] lg:pt-24"
	>
		<div class="anim-rise flex flex-col items-start gap-6">
			<Eyebrow>{hero.eyebrow}</Eyebrow>
			<h1 class="font-display font-bold text-display text-ink text-balance">
				{hero.title[0]}<br /><span class="text-primary">{hero.title[1]}</span>
			</h1>
			<p class="max-w-xl text-lg text-ink-mute text-pretty">{hero.lede}</p>
			<div class="flex flex-wrap items-center gap-3 pt-1">
				<Button href={hero.primaryCta.href} variant="primary" size="lg">
					{hero.primaryCta.label}
					<ArrowIcon />
				</Button>
				<Button href={hero.secondaryCta.href} variant="default" style="outline" size="lg">
					{hero.secondaryCta.label}
				</Button>
			</div>
			<div
				class="mt-2 inline-flex items-center gap-3 rounded-lg border border-paper-edge bg-paper-soft px-4 py-2.5 font-mono text-sm"
			>
				<span class="text-primary">$</span>
				<span class="text-ink">{hero.install}</span>
			</div>
		</div>
		<div class="anim-rise lg:pl-4" style="animation-delay: 0.12s">
			<Terminal file={hero.terminal.file} lines={hero.terminal.lines} />
		</div>
	</div>
</section>
