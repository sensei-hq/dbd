<script lang="ts">
	import Seo from '$lib/components/Seo.svelte';
	import HeroSection from '$lib/components/HeroSection.svelte';
	import OverviewSection from '$lib/components/OverviewSection.svelte';
	import ConceptsSection from '$lib/components/ConceptsSection.svelte';
	import TargetsSection from '$lib/components/TargetsSection.svelte';
	import CommandsSection from '$lib/components/CommandsSection.svelte';
	import AudienceSection from '$lib/components/AudienceSection.svelte';
	import StartSection from '$lib/components/StartSection.svelte';
	import { SITE_URL } from '$lib/seo';

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

<HeroSection />
<OverviewSection />
<ConceptsSection />
<TargetsSection />
<CommandsSection />
<AudienceSection />
<StartSection />
