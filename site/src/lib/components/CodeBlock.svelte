<script lang="ts">
	import { highlight } from '$lib/highlight';
	import type { Code } from '$lib/data';

	let { code, class: className = '' }: { code: Code; class?: string } = $props();
	const lines = $derived(highlight(code.source));
</script>

<div class="overflow-hidden rounded-xl border border-line bg-paper-2 {className}">
	<div class="flex items-center justify-between border-b border-paper-edge px-4 py-2.5">
		<div class="flex items-center gap-1.5">
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
			<span class="terminal-dot h-2.5 w-2.5 rounded-full bg-paper-mute"></span>
		</div>
		<span class="font-mono text-xs text-ink-soft">{code.label}</span>
	</div>
	<pre class="overflow-x-auto px-4 py-4 font-mono text-[0.82rem] leading-relaxed"><code
			>{#each lines as toks, i (i)}<div class="whitespace-pre">{#each toks as tk, j (j)}<span
							class={tk.cls}>{tk.t}</span
						>{/each}{toks.length === 0 ? '\n' : ''}</div>{/each}</code
		></pre>
</div>
