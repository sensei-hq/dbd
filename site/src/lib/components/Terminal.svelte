<script lang="ts">
	import type { TerminalLine } from '$lib/data';

	let { file, lines }: { file: string; lines: TerminalLine[] } = $props();

	const lineColor: Record<TerminalLine['type'], string> = {
		cmd: 'text-ink',
		out: 'text-ink-mute',
		ok: 'text-success'
	};
</script>

<div class="overflow-hidden rounded-xl border border-line bg-paper-2 shadow-2xl">
	<div class="flex items-center justify-between border-b border-paper-edge px-4 py-2.5">
		<div class="flex items-center gap-1.5">
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
		</div>
		<span class="font-mono text-xs text-ink-soft">{file}</span>
	</div>
	<div class="px-4 py-4 font-mono text-[0.82rem] leading-relaxed">
		{#each lines as ln, i (i)}
			<div class="flex gap-2 whitespace-pre">
				<span class={ln.type === 'cmd' ? 'text-primary' : 'text-transparent'}>$</span>
				<span class={lineColor[ln.type]}>{ln.text}</span>
			</div>
		{/each}
	</div>
</div>
