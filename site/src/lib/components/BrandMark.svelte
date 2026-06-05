<script lang="ts">
	let { class: className = 'h-8 w-8' }: { class?: string } = $props();
</script>

<svg viewBox="0 0 512 512" class={className} fill="none" aria-hidden="true">
	<defs>
		<linearGradient id="wmHex" x1="92" y1="161" x2="420" y2="351" gradientUnits="userSpaceOnUse">
			<stop offset="0" stop-color="#38BDF8" />
			<stop offset="0.5" stop-color="#37B6A6" />
			<stop offset="1" stop-color="#3FD168" />
		</linearGradient>
	</defs>
	<path
		d="M256 64 L420 159 L420 353 L256 448 L92 353 L92 159 Z"
		stroke="url(#wmHex)"
		stroke-width="30"
		stroke-linejoin="round"
		stroke-linecap="round"
	/>
	<g transform="translate(122 128) scale(8.9)" color="#38BDF8">
		<path fill="currentColor" d="M12 10c4.418 0 8-1.79 8-4s-3.582-4-8-4s-8 1.79-8 4s3.582 4 8 4" />
		<path fill="currentColor" opacity="0.5" d="M4 12v6c0 2.21 3.582 4 8 4s8-1.79 8-4v-6c0 2.21-3.582 4-8 4s-8-1.79-8-4" />
		<path fill="currentColor" opacity="0.7" d="M4 6v6c0 2.21 3.582 4 8 4s8-1.79 8-4V6c0 2.21-3.582 4-8 4S4 8.21 4 6" />
	</g>
	<g transform="translate(214 198) scale(6)" color="#3FD168" stroke="#10151F" stroke-width="0.9" paint-order="stroke">
		<circle cx="21" cy="26" r="2" fill="currentColor" />
		<circle cx="21" cy="6" r="2" fill="currentColor" />
		<circle cx="4" cy="16" r="2" fill="currentColor" />
		<path fill="currentColor" d="M28 12a3.996 3.996 0 0 0-3.858 3h-4.284a3.966 3.966 0 0 0-5.491-2.643l-3.177-3.97A3.96 3.96 0 0 0 12 6a4 4 0 1 0-4 4a4 4 0 0 0 1.634-.357l3.176 3.97a3.924 3.924 0 0 0 0 4.774l-3.176 3.97A4 4 0 0 0 8 22a4 4 0 1 0 4 4a3.96 3.96 0 0 0-.81-2.387l3.176-3.97A3.966 3.966 0 0 0 19.858 17h4.284A3.993 3.993 0 1 0 28 12M6 6a2 2 0 1 1 2 2a2 2 0 0 1-2-2m2 22a2 2 0 1 1 2-2a2 2 0 0 1-2 2m8-10a2 2 0 1 1 2-2a2 2 0 0 1-2 2m12 0a2 2 0 1 1 2-2a2 2 0 0 1-2 2" />
	</g>
</svg>
