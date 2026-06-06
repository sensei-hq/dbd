<script lang="ts">
	import { Button } from '@rokkit/ui';
	import Eyebrow from '$lib/components/Eyebrow.svelte';
	import SectionHead from '$lib/components/SectionHead.svelte';
	import CodeBlock from '$lib/components/CodeBlock.svelte';
	import Terminal from '$lib/components/Terminal.svelte';
	import FeatureCard from '$lib/components/FeatureCard.svelte';
	import TargetCard from '$lib/components/TargetCard.svelte';
	import ArrowIcon from '$lib/components/ArrowIcon.svelte';
	import { hero, overview, concepts, targets, audience, start } from '$lib/data';
</script>

<svelte:head>
	<title>dbd — Database schemas as code</title>
	<meta
		name="description"
		content="dbd turns plain SQL DDL files into a versioned, deployable schema. No DSL, no ORM — the folder structure is the source of truth. Built in Rust."
	/>
</svelte:head>

<!-- Hero -->
<section id="top" class="relative overflow-hidden">
	<div class="pointer-events-none absolute inset-0 bg-grid mask-fade-b opacity-60"></div>
	<div
		class="relative mx-auto grid max-w-content items-center gap-12 px-6 pb-section pt-16 lg:grid-cols-[1.05fr_0.95fr] lg:pt-24"
	>
		<div class="anim-rise flex flex-col items-start gap-6">
			<Eyebrow>{hero.eyebrow}</Eyebrow>
			<h1 class="font-display font-bold text-display text-surface-z9 text-balance">
				{hero.title[0]}<br /><span class="text-primary-z5">{hero.title[1]}</span>
			</h1>
			<p class="max-w-xl text-lg text-surface-z7 text-pretty">{hero.lede}</p>
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
				class="mt-2 inline-flex items-center gap-3 rounded-lg border border-surface-z3 bg-surface-z1 px-4 py-2.5 font-mono text-sm"
			>
				<span class="text-primary-z5">$</span>
				<span class="text-surface-z9">{hero.install}</span>
			</div>
		</div>
		<div class="anim-rise lg:pl-4" style="animation-delay: 0.12s">
			<Terminal file={hero.terminal.file} lines={hero.terminal.lines} />
		</div>
	</div>
</section>

<!-- Overview -->
<section id="overview" class="grid-section bg-surface-z1">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={overview.eyebrow} title={overview.title} lede={overview.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{#each overview.features as f (f.tag)}
				<FeatureCard tag={f.tag} title={f.title} body={f.body} />
			{/each}
		</div>
	</div>
</section>

<!-- Concepts -->
<section id="concepts" class="grid-section">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={concepts.eyebrow} title={concepts.title} align="center" />
		<div class="mt-14 flex flex-col gap-16">
			{#each concepts.items as item, i (item.id)}
				<div class="grid items-center gap-8 lg:grid-cols-2 lg:gap-14">
					<div class="flex flex-col gap-4 {i % 2 === 1 ? 'lg:order-2' : ''}">
						<div class="flex items-center gap-3">
							<span
								class="grid h-7 w-7 place-items-center rounded-md border border-primary-z4 bg-primary-z1 font-mono text-xs text-primary-z5"
							>
								{String(i + 1).padStart(2, '0')}
							</span>
							<span class="font-mono text-label uppercase text-surface-z5">{item.kicker}</span>
						</div>
						<h3 class="font-display font-semibold text-h3 text-surface-z9 text-balance">{item.title}</h3>
						<p class="text-base text-surface-z7 text-pretty">{item.body}</p>
					</div>
					<div class={i % 2 === 1 ? 'lg:order-1' : ''}>
						<CodeBlock code={item.code} />
					</div>
				</div>
			{/each}
		</div>
	</div>
</section>

<!-- Targets -->
<section id="targets" class="grid-section bg-surface-z1">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={targets.eyebrow} title={targets.title} lede={targets.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each targets.items as t (t.name)}
				<TargetCard name={t.name} scheme={t.scheme} body={t.body} notes={t.notes} />
			{/each}
		</div>
	</div>
</section>

<!-- Audience -->
<section id="audience" class="grid-section">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={audience.eyebrow} title={audience.title} align="center" />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each audience.items as a (a.title)}
				<div class="card flex flex-col gap-2 rounded-xl p-6">
					<h3 class="font-display font-medium text-lg text-surface-z9">{a.title}</h3>
					<p class="text-sm text-surface-z7 text-pretty">{a.body}</p>
				</div>
			{/each}
		</div>
	</div>
</section>

<!-- Get started -->
<section id="start" class="grid-section bg-surface-z1">
	<div class="mx-auto max-w-content px-6 py-section">
		<div class="card overflow-hidden rounded-xl" style="border-color: var(--color-primary-400)">
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
						<div class="flex items-center gap-4 rounded-lg border border-surface-z3 bg-surface-z0 px-5 py-4">
							<span
								class="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-primary-z1 font-mono text-sm text-primary-z5"
							>
								{s.n}
							</span>
							<div class="flex flex-col">
								<span class="text-xs text-surface-z5">{s.text}</span>
								<code class="font-mono text-sm text-surface-z9"><span class="text-primary-z5">$ </span>{s.code}</code>
							</div>
						</div>
					{/each}
				</div>
			</div>
		</div>
	</div>
</section>
