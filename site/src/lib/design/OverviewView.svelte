<script lang="ts">
  /**
   * The project at a glance — the root's first tab (#28).
   *
   * What the project is made of, counted and iconed the way the diagram draws each kind;
   * the project note in full; and the same counts per schema. The numbers come from
   * `overview.ts`, which derives them from the model and nothing else.
   */
  import Markdown from './Markdown.svelte';
  import { noteBlocks } from './md';
  import { databaseLabel, overviewCounts, schemaRows, OVERVIEW_ICONS } from './overview';
  import { newestFirst, summaryParts, versionDate, versionLabel, versionSummary } from './changelog';
  import type { SchemaModel } from './model';

  let { model, onTab }: { model: SchemaModel; onTab?: (id: string) => void } = $props();

  // The latest three — the overview's glance at the changelog, not a second copy of it.
  const recent = $derived(newestFirst(model.history ?? []).slice(0, 3));

  const counts = $derived(overviewCounts(model));
  const schemas = $derived(schemaRows(model));
  const note = $derived(noteBlocks(model.project.note));
  const hasEntities = $derived(model.entities !== undefined);
</script>

<div data-overview class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
  <div class="px-6 py-6">
    <section data-section="counts">
      <h2 class="font-mono text-label uppercase text-faint">At a glance</h2>
      <div class="mt-3 flex flex-wrap items-center gap-2 text-sm text-muted">
        <span class="{OVERVIEW_ICONS.database} text-base text-faint" aria-hidden="true"></span>
        <span>{databaseLabel(model.project.db)}</span>
        {#if model.version}<span class="ds-badge">schema model v{model.version}</span>{/if}
      </div>
      <div class="mt-4 grid gap-3" style="grid-template-columns: repeat(auto-fill, minmax(9.5rem, 1fr));">
        {#each counts as c (c.key)}
          <div data-count={c.key} class="rounded-app border border-line bg-paper px-4 py-3">
            <span data-count-icon class="{c.icon} text-lg text-accent-2" aria-hidden="true"></span>
            <div class="mt-2 flex items-baseline gap-1.5">
              <span class="font-display text-h3 font-semibold leading-none text-fg">{c.count}</span>
              <span class="text-xs text-muted">{c.label}</span>
            </div>
          </div>
        {/each}
      </div>
    </section>

    <section data-section="notes" class="mt-8">
      <h2 class="font-mono text-label uppercase text-faint">Notes</h2>
      <div class="mt-3 flex max-w-3xl flex-col gap-2 text-sm leading-relaxed text-muted">
        {#if note.length}
          <Markdown blocks={note} />
        {:else}
          <p class="text-faint">
            No project note — set <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;"
              >project.note</code
            > in design.yaml.
          </p>
        {/if}
      </div>
    </section>

    {#if recent.length}
      <section data-section="recent" class="mt-8">
        <h2 class="font-mono text-label uppercase text-faint">Recent changes</h2>
        <div class="mt-3 flex max-w-3xl flex-col">
          {#each recent as entry (entry.version)}
            {@const parts = summaryParts(versionSummary(entry)).filter((p) => p.op !== 'fields')}
            <div
              data-version={entry.version}
              class="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line-soft py-2.5 text-sm"
            >
              <span class="ds-badge">{versionLabel(entry)}</span>
              <span class="text-fg">{entry.description || 'Untitled version'}</span>
              <span class="ml-auto font-mono text-xs text-faint">
                {#if entry.baseline}baseline{:else if parts.length}{parts
                    .map((p) => p.text.split(' ')[0])
                    .join(' ')}{:else}no changes{/if} · {versionDate(entry)}
              </span>
            </div>
          {/each}
        </div>
        <button
          type="button"
          class="mt-3 font-mono text-xs text-accent-2 hover:underline"
          onclick={() => onTab?.('changelog')}>View the full changelog →</button
        >
      </section>
    {/if}

    <section data-section="schemas" class="mt-8">
      <h2 class="font-mono text-label uppercase text-faint">
        Schemas <span class="text-faint">· {schemas.length}</span>
      </h2>
      <table class="mt-3 w-full max-w-3xl table-fixed border-collapse text-left">
        <thead>
          <tr class="font-mono text-xs uppercase tracking-wider text-faint">
            <th class="ds-th py-2.5 pr-4 font-medium">Schema</th>
            <th class="ds-th py-2.5 pr-4 font-medium">Tables</th>
            <th class="ds-th py-2.5 pr-4 font-medium">Enums</th>
            {#if hasEntities}<th class="ds-th py-2.5 font-medium">Views &amp; routines</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each schemas as s (s.name)}
            <tr data-schema-row={s.name} class="border-b border-line-soft font-mono text-xs text-muted">
              <td class="py-2.5 pr-4">
                <span class="{OVERVIEW_ICONS.schemas} mr-1.5 align-[-2px] text-faint" aria-hidden="true"></span
                ><span class="font-semibold text-fg">{s.name}</span>
              </td>
              <td class="py-2.5 pr-4">{s.tables}</td>
              <td class="py-2.5 pr-4">{s.enums || '—'}</td>
              {#if hasEntities}<td class="py-2.5">{s.other || '—'}</td>{/if}
            </tr>
          {/each}
        </tbody>
      </table>
    </section>
  </div>
</div>
