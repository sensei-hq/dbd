<script lang="ts">
  /**
   * One table's or enum's changelog (#33): the versions that changed it, newest first, each
   * with what happened to it and the edits inside. The project changelog narrowed by
   * `entityHistory`, so the two can never disagree.
   */
  import FieldEdits from './FieldEdits.svelte';
  import { entityHistory, OP_CLASS, OP_MARK, versionDate, versionLabel } from './changelog';
  import type { SchemaModel } from './model';

  let { model, kind, schema, name }: { model: SchemaModel; kind: string; schema: string; name: string } = $props();

  // Snapshots record tables and enums; every other kind has no history to narrow.
  const tracked = $derived(kind === 'table' || kind === 'enum');
  const history = $derived(
    tracked ? entityHistory(model.history ?? [], kind as 'table' | 'enum', schema, name) : { rows: [] },
  );
</script>

<div data-entity-changelog class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
  <div class="px-6 py-6">
    <h2 class="font-mono text-label uppercase text-faint">Changelog</h2>
    {#if !tracked}
      <p class="mt-3 max-w-2xl text-sm text-faint">
        No history for a {kind.replace(/_/g, ' ')} yet — snapshots record tables and enums only, so
        what changed in views and routines is not kept.
      </p>
    {:else if !model.history?.length}
      <p class="mt-3 max-w-2xl text-sm text-faint">
        No snapshots yet, so there is no history to show. It begins with the first snapshot —
        <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">dbd release</code>
        cuts it.
      </p>
    {:else}
      <div class="mt-3 flex max-w-4xl flex-col">
        {#each history.rows as { entry, change } (entry.version)}
          <div data-version={entry.version} data-op={change.op} class="border-b border-line-soft py-3">
            <div class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
              <span class="ds-badge">{versionLabel(entry)}</span>
              <span class="font-mono text-sm {OP_CLASS[change.op]}" aria-label={change.op}>{OP_MARK[change.op]}</span>
              <span class="text-sm text-fg">{entry.description || 'Untitled version'}</span>
              <span class="font-mono text-xs text-faint">{change.op}</span>
              <span class="ml-auto font-mono text-xs text-faint">{versionDate(entry)}</span>
            </div>
            {#if change.fields.length}
              <div class="ml-1 mt-2 border-l border-line-soft pl-4">
                <FieldEdits fields={change.fields} />
              </div>
            {/if}
          </div>
        {/each}
        {#if history.sinceBaseline !== undefined}
          <p class="py-3 text-sm text-faint">
            Present since v{history.sinceBaseline}, the baseline{history.rows.length ? '' : ' — no version has changed it since'}.
          </p>
        {/if}
      </div>
    {/if}
  </div>
</div>
