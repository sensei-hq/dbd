<script lang="ts">
	import { Button } from '@rokkit/ui';
	import Eyebrow from '$lib/components/Eyebrow.svelte';
	import SectionHead from '$lib/components/SectionHead.svelte';
	import CodeBlock from '$lib/components/CodeBlock.svelte';
	import Terminal from '$lib/components/Terminal.svelte';
	import FeatureCard from '$lib/components/FeatureCard.svelte';
	import TargetCard from '$lib/components/TargetCard.svelte';
	import CommandCard from '$lib/components/CommandCard.svelte';
	import ArrowIcon from '$lib/components/ArrowIcon.svelte';
	import Seo from '$lib/components/Seo.svelte';
	import { SITE_URL } from '$lib/seo';
	import { hero, overview, concepts, targets, commands, audience, start } from '$lib/data';

	const description =
		'dbd turns plain SQL DDL files into a versioned, deployable schema. No DSL, no ORM — the folder structure is the source of truth. Built in Rust.';

	// JSON-LD structured data (rich results): the site + the dbd CLI/library.
	const jsonLd = {
		'@context': 'https://schema.org',
		'@graph': [
			{ '@type': 'WebSite', '@id': `${SITE_URL}/#website`, name: 'dbd', url: `${SITE_URL}/` },
			{
				'@type': 'SoftwareApplication',
				'@id': `${SITE_URL}/#app`,
				name: 'dbd',
				alternateName: 'Database Designer',
				applicationCategory: 'DeveloperApplication',
				operatingSystem: 'macOS, Linux, Windows',
				url: `${SITE_URL}/`,
				description,
				softwareHelp: `${SITE_URL}/guide`,
				offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' }
			}
		]
	};
</script>

<Seo title="dbd — Database schemas as code" {description} />
<svelte:head>
	<!-- eslint-disable-next-line svelte/no-at-html-tags — static, developer-authored JSON-LD -->
	{@html `<script type="application/ld+json">${JSON.stringify(jsonLd)}</` + `script>`}
</svelte:head>

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

<!-- Overview -->
<section id="overview" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={overview.eyebrow} title={overview.title} lede={overview.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{#each overview.features as f (f.title)}
				<FeatureCard title={f.title} body={f.body} />
			{/each}
		</div>
	</div>
</section>

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

<!-- Targets -->
<section id="targets" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={targets.eyebrow} title={targets.title} lede={targets.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each targets.items as t (t.name)}
				<TargetCard name={t.name} scheme={t.scheme} body={t.body} notes={t.notes} />
			{/each}
		</div>
	</div>
</section>

<!-- Commands -->
<section id="commands">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={commands.eyebrow} title={commands.title} lede={commands.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2">
			{#each commands.items as c (c.cmd)}
				<CommandCard cmd={c.cmd} body={c.body} />
			{/each}
		</div>
	</div>
</section>

<!-- Audience -->
<section id="audience" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={audience.eyebrow} title={audience.title} align="center" />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each audience.items as a (a.title)}
				<div class="flex flex-col gap-2 rounded-xl border border-paper-edge bg-paper-mute p-6">
					<h3 class="font-display font-medium text-lg text-ink">{a.title}</h3>
					<p class="text-sm text-ink-mute text-pretty">{a.body}</p>
				</div>
			{/each}
		</div>
	</div>
</section>

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
						<div class="flex items-center gap-4 rounded-lg border border-paper-edge bg-paper px-5 py-4">
							<span
								class="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-accent-soft font-mono text-sm text-primary"
							>
								{s.n}
							</span>
							<div class="flex flex-col">
								<span class="text-xs text-ink-soft">{s.text}</span>
								<code class="font-mono text-sm text-ink"><span class="text-primary">$ </span>{s.code}</code>
							</div>
						</div>
					{/each}
				</div>
			</div>
		</div>
	</div>
</section>
