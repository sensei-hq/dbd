<script lang="ts">
  /**
   * A DDL comment, rendered: paragraphs, bullet lists and `inline code` — what `md.ts` parses.
   *
   * One renderer, so every comment on the site reads the same: the entities list, a table's
   * Details tab, its column notes and the project overview. It renders bare blocks; the caller
   * owns the wrapper and its spacing.
   *
   * Keyed by position throughout. A comment may name the same `code` twice — legal text — and
   * keying segments by their own text crashed on it with each_key_duplicate.
   */
  import type { Block, Seg } from './md';

  let { blocks }: { blocks: Block[] } = $props();
</script>

{#snippet segs(parts: Seg[])}
  {#each parts as part, i (i)}
    {#if part.code}
      <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">{part.text}</code>
    {:else}
      {part.text}
    {/if}
  {/each}
{/snippet}

{#each blocks as block, i (i)}
  {#if block.type === 'ul'}
    <ul class="flex list-disc flex-col gap-1 pl-5">
      {#each block.lines as line, j (j)}<li>{@render segs(line)}</li>{/each}
    </ul>
  {:else}
    <p>{@render segs(block.lines[0])}</p>
  {/if}
{/each}
