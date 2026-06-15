<script lang="ts">
  import { nodeId, type SchemaModel, type Table } from '$lib/design/model';
  import { noteBlocks as blocks, type Seg } from './md';

  let { model, onNav }: { model: SchemaModel; onNav?: (key: string) => void } = $props();

  function refCount(t: Table): number {
    return model.refs.filter(
      (r) =>
        (r.from.s === t.schema && r.from.t === t.name) || (r.to.s === t.schema && r.to.t === t.name)
    ).length;
  }
</script>

{#snippet segs(parts: Seg[])}
  {#each parts as part (part.text)}
    {#if part.code}
      <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">{part.text}</code>
    {:else}
      {part.text}
    {/if}
  {/each}
{/snippet}

<div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
  <div class="mx-auto max-w-5xl px-6 pb-6">
    <table class="w-full table-fixed border-collapse text-left">
      <colgroup>
        <col style="width: 200px;" />
        <col style="width: 58px;" />
        <col style="width: 58px;" />
        <col />
      </colgroup>
      <thead>
        <tr class="font-mono text-xs uppercase tracking-wider text-faint">
          <th class="ds-th py-3 pr-4 font-medium">Entity</th>
          <th class="ds-th py-3 pr-4 font-medium">Cols</th>
          <th class="ds-th py-3 pr-4 font-medium">Refs</th>
          <th class="ds-th py-3 font-medium">Comment</th>
        </tr>
      </thead>
      <tbody>
        {#each model.tables as t (nodeId(t.schema, t.name))}
          {@const rc = refCount(t)}
          <tr
            class="group cursor-pointer border-b border-line-soft align-top hover:bg-paper-2"
            onclick={() => onNav?.(nodeId(t.schema, t.name))}
          >
            <td class="py-3.5 pr-4">
              <div class="font-mono text-xs text-faint" style="overflow-wrap: anywhere;">{t.schema}.</div>
              <div
                class="font-display text-sm font-semibold text-fg group-hover:text-accent-2"
                style="overflow-wrap: anywhere;"
              >
                {t.name}
              </div>
            </td>
            <td class="py-3.5 pr-4 font-mono text-xs text-muted">{t.columns.length}</td>
            <td class="py-3.5 pr-4 font-mono text-xs text-muted">{rc || '—'}</td>
            <td class="py-3.5 text-sm">
              <div class="flex max-w-2xl flex-col gap-2 text-sm leading-relaxed text-muted">
                {#each blocks(t.noteMd) as block (block)}
                  {#if block.type === 'ul'}
                    <ul class="flex list-disc flex-col gap-1 pl-5">
                      {#each block.lines as line (line)}
                        <li>{@render segs(line)}</li>
                      {/each}
                    </ul>
                  {:else}
                    <p>{@render segs(block.lines[0])}</p>
                  {/if}
                {/each}
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>
