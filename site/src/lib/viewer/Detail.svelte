<script lang="ts">
  import { marked } from 'marked';
  import Diagram from './Diagram.svelte';
  import { createViewerState } from './state.svelte';
  import { nodeId, type SchemaModel } from './model';

  let { model, selected }: { model: SchemaModel; selected: string | null } = $props();

  // Internal focus state drives the mini ERD AND the columns/note. It is seeded
  // from the `selected` prop, but clicking a neighbor in the mini ERD updates
  // `mini.selected` directly so the whole panel re-focuses consistently.
  const mini = createViewerState();
  mini.mode = 'focus';
  // Reset when the prop changes. This effect depends only on `selected`, so a
  // neighbor click (which sets `mini.selected` to a value not equal to the prop)
  // is NOT reverted — the effect doesn't re-run until the prop itself changes.
  $effect(() => {
    mini.selected = selected;
    mini.mode = 'focus';
  });

  const current = $derived(model.tables.find((t) => nodeId(t.schema, t.name) === mini.selected) ?? null);

  // FK columns of the current table = columns that appear as `from.c` in its refs.
  const fkCols = $derived(
    new Set(
      current
        ? model.refs
            .filter((r) => r.from.s === current.schema && r.from.t === current.name)
            .map((r) => r.from.c)
        : []
    )
  );

  // The note is the user's own local DDL comment — trusted input for this
  // offline tool — so rendering it via {@html} below is acceptable.
  // marked.parse is synchronous (returns a string) with no async extensions
  // configured; cast to string to satisfy TS's string | Promise<string> union.
  const noteHtml = $derived(
    current && (current.noteMd ?? current.note)
      ? (marked.parse(current.noteMd ?? current.note ?? '') as string)
      : ''
  );

  type Badge = { label: string; cls: string };
  // PK/FK use the accent chip; NN/ENUM the muted chip.
  function badgesFor(name: string, pk?: boolean, nn?: boolean, en?: boolean): Badge[] {
    const out: Badge[] = [];
    if (pk) out.push({ label: 'PK', cls: 'bg-accent-soft text-primary' });
    if (fkCols.has(name)) out.push({ label: 'FK', cls: 'bg-accent-soft text-primary' });
    if (nn) out.push({ label: 'NN', cls: 'bg-paper-mute text-ink-mute' });
    if (en) out.push({ label: 'ENUM', cls: 'bg-paper-mute text-ink-mute' });
    return out;
  }
</script>

{#if current}
  <section class="vw-detail h-full overflow-y-auto bg-paper text-ink">
    <div class="px-6 py-6">
      <!-- header -->
      <header class="mb-6">
        <div class="font-mono text-xs text-ink-faint">{current.schema}.</div>
        <div class="flex items-center gap-3">
          <h2 class="font-display text-lg font-semibold text-ink">{current.name}</h2>
          <span class="vw-badge bg-accent-soft text-primary">{current.columns.length} columns</span>
        </div>
      </header>

      <!-- note (only when present) -->
      {#if noteHtml}
        <section class="mb-8">
          <h3 class="font-mono uppercase text-ink-faint text-xs">Comment</h3>
          <!-- trusted local DDL comment, see noteHtml above -->
          <div class="vw-md text-ink-mute text-sm mt-2">{@html noteHtml}</div>
        </section>
      {/if}

      <!-- columns -->
      <section class="mb-8">
        <h3 class="font-mono uppercase text-ink-faint text-xs">Columns</h3>
        <table class="vw-cols mt-3 w-full border-collapse text-left">
          <thead>
            <tr class="font-mono uppercase text-ink-faint text-xs">
              <th class="py-2.5 pr-4 font-medium">Column</th>
              <th class="py-2.5 pr-4 font-medium">Props</th>
              <th class="py-2.5 font-medium">Type</th>
            </tr>
          </thead>
          <tbody data-cols>
            {#each current.columns as c (c.name)}
              <tr data-col-row={c.name} class="border-t border-paper-edge align-top">
                <td class="py-2.5 pr-4">
                  <div class="font-mono text-ink">{c.name}</div>
                  {#if c.note}
                    <div class="text-ink-mute text-xs mt-0.5">{c.note}</div>
                  {/if}
                  {#if c.def}
                    <div class="vw-col-def font-mono text-ink-faint mt-0.5">default: {c.def}</div>
                  {/if}
                </td>
                <td class="py-2.5 pr-4">
                  <span class="flex flex-wrap gap-1">
                    {#each badgesFor(c.name, c.pk, c.nn, c.en) as b (b.label)}
                      <span class="vw-badge {b.cls}">{b.label}</span>
                    {/each}
                  </span>
                </td>
                <td class="vw-col-type py-2.5 font-mono text-ink-mute">{c.type}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>

      <!-- mini focused ERD -->
      <section>
        <h3 class="font-mono uppercase text-ink-faint text-xs mb-2">Relationships</h3>
        <div class="vw-mini-erd relative h-64 rounded-md border border-paper-edge overflow-hidden">
          <Diagram {model} state={mini} />
        </div>
      </section>
    </div>
  </section>
{:else}
  <section class="vw-detail h-full bg-paper text-ink-faint">
    <div class="px-6 py-6 text-sm">Select a table</div>
  </section>
{/if}

<style>
  /* structural-only rules; all colors come from Rokkit tokens in markup */
  .vw-badge {
    display: inline-flex;
    align-items: center;
    border-radius: var(--radius-full, 9999px);
    padding: 0 0.4rem;
    font-family: 'IBM Plex Mono', ui-monospace, monospace;
    font-size: 0.6rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  /* fold the old default cell under the column name (mockup uses text-[0.66rem]) */
  .vw-col-def {
    font-size: 0.66rem;
  }
  /* long types like TIMESTAMP WITH TIME ZONE wrap cleanly instead of overflowing */
  .vw-col-type {
    overflow-wrap: anywhere;
  }
  .vw-md :global(strong) {
    font-weight: 600;
    color: var(--ink);
  }
  .vw-md :global(p) {
    margin: 0.25rem 0;
  }
  .vw-md :global(code) {
    font-family: 'IBM Plex Mono', ui-monospace, monospace;
    font-size: 0.85em;
  }
</style>
