<script lang="ts">
  /**
   * What changed in each version, newest first (#29).
   *
   * One card per version: its number, description and date; how many tables and enums it
   * added, modified or removed; and each of them, with the column, index, constraint and
   * enum-value edits inside a modified one a click away. The history is computed by dbd
   * from `snapshots/` (`crates/dbd-core/src/history.rs`) — this only renders it.
   */
  import { OVERVIEW_ICONS } from './overview';
  import { newestFirst, OP_MARK, summaryParts, versionDate, versionLabel, versionSummary } from './changelog';
  import type { ChangeOp, FieldEdit, SchemaModel } from './model';

  let { model, onNav }: { model: SchemaModel; onNav?: (key: string) => void } = $props();

  const entries = $derived(newestFirst(model.history ?? []));

  // Two hues on this site, by design: jade for what arrived, sky for what changed, and a
  // struck-through muted name for what left.
  const OP_CLASS: Record<ChangeOp, string> = {
    added: 'text-success',
    modified: 'text-accent-2',
    renamed: 'text-accent-2',
    removed: 'text-faint',
  };

  const PART_CLASS: Record<string, string> = {
    added: 'text-success',
    modified: 'text-accent-2',
    removed: '',
    fields: 'text-faint',
  };

  const isTable = (schema: string, name: string) =>
    model.tables.some((t) => t.schema === schema && t.name === name);

  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

  /** The definition side of an edit, as one line: `from → to`, or whichever side exists. */
  function detail(f: FieldEdit): string {
    if (f.op === 'renamed') return `${f.from} → ${f.to}`;
    if (f.from && f.to) return `${f.from} → ${f.to}`;
    return f.to ?? f.from ?? f.note ?? '';
  }
</script>

<div data-changelog class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
  <div class="px-6 py-6">
    {#if entries.length === 0}
      <section class="max-w-2xl text-sm leading-relaxed text-muted">
        <h2 class="font-mono text-label uppercase text-faint">Changelog</h2>
        <p class="mt-3">
          No snapshots yet, so there is no changelog. History begins with the first snapshot —
          <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">dbd release</code>
          cuts it — and every
          <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">dbd snapshot</code>
          after that adds a version. A project still kept in step with
          <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">dbd reconcile</code>
          has none.
        </p>
      </section>
    {:else}
      <div class="flex max-w-4xl flex-col gap-4">
        {#each entries as entry (entry.version)}
          {@const parts = summaryParts(versionSummary(entry))}
          <article data-version={entry.version} class="rounded-app border border-line bg-paper">
            <header class="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line-soft px-5 py-3">
              <span class="ds-badge ds-badge-accent">{versionLabel(entry)}</span>
              <h2 class="font-display text-base font-semibold text-fg">{entry.description || 'Untitled version'}</h2>
              <span class="ml-auto font-mono text-xs text-faint">{versionDate(entry)}</span>
            </header>

            <div class="px-5 py-3">
              {#if entry.baseline}
                <p class="text-sm text-muted">
                  History begins here: {plural(entry.baseline.tables, 'table', 'tables')} · {plural(
                    entry.baseline.enums,
                    'enum',
                    'enums',
                  )}.
                </p>
              {:else if entry.changes.length === 0}
                <p class="text-sm text-faint">No table or enum changes.</p>
              {:else}
                <p class="font-mono text-xs text-muted">
                  {#each parts as part, i (part.op)}{i > 0 ? ' · ' : ''}<span class={PART_CLASS[part.op]}
                      >{part.text}</span
                    >{/each}
                </p>
                <ul class="mt-2 flex flex-col">
                  {#each entry.changes as c (c.kind + ':' + c.schema + '.' + c.name)}
                    <li data-change="{c.schema}.{c.name}" data-op={c.op} class="border-b border-line-soft last:border-0">
                      {#snippet row()}
                        <span class="w-3 font-mono text-sm {OP_CLASS[c.op]}" aria-label={c.op}>{OP_MARK[c.op]}</span>
                        <span
                          class="{c.kind === 'enum' ? OVERVIEW_ICONS.enums : OVERVIEW_ICONS.tables} text-faint"
                          aria-hidden="true"
                        ></span>
                        <span class="font-mono text-xs text-faint">{c.kind}</span>
                        <span class="font-mono text-xs font-semibold {c.op === 'removed' ? 'text-faint line-through' : 'text-fg'}"
                          >{c.schema}.{c.name}</span
                        >
                        {#if c.fields.length}<span class="ml-auto font-mono text-xs text-faint"
                            >{plural(c.fields.length, 'field', 'fields')}</span
                          >{/if}
                      {/snippet}
                      {#if c.fields.length}
                        <details class="group">
                          <summary class="flex cursor-pointer list-none items-center gap-2 py-2 hover:bg-paper-2">
                            {@render row()}
                          </summary>
                          <ul class="mb-2 ml-5 flex flex-col gap-1 border-l border-line-soft pl-4">
                            {#each c.fields as f, i (i)}
                              <li data-field-op={f.op} class="flex flex-wrap items-baseline gap-2 font-mono text-xs text-muted">
                                <span class="w-3 {OP_CLASS[f.op]}">{OP_MARK[f.op]}</span>
                                <span class="text-faint">{f.kind}</span>
                                <!-- A rename is its own `old → new`; naming the new one first says it twice. -->
                                {#if f.op !== 'renamed'}<span class={f.op === 'removed' ? 'text-faint line-through' : 'text-fg'}
                                    >{f.name}</span
                                  >{/if}
                                {#if detail(f)}<span class={f.op === 'renamed' ? 'text-fg' : ''} style="overflow-wrap: anywhere;"
                                    >{detail(f)}</span
                                  >{/if}
                              </li>
                            {/each}
                          </ul>
                        </details>
                      {:else}
                        <div class="flex items-center gap-2 py-2">
                          {@render row()}
                          {#if c.op !== 'removed' && c.kind === 'table' && isTable(c.schema, c.name)}
                            <button
                              type="button"
                              class="ml-auto font-mono text-xs text-accent-2 hover:underline"
                              onclick={() => onNav?.(`${c.schema}.${c.name}`)}>open</button
                            >
                          {/if}
                        </div>
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </div>
</div>
