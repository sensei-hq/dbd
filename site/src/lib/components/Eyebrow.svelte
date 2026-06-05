<script lang="ts">
	let { children } = $props();
</script>

<div class="flex items-center gap-2.5 text-label font-mono font-medium uppercase text-primary-z5">
	<span class="inline-block h-1.5 w-1.5 rounded-full bg-primary-z5"></span>
	{@render children()}
</div>
