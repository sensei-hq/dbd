<script lang="ts">
  import { marked } from 'marked';
  import { nodeId, type SchemaModel } from './model';

  // Ports docs/mockup/designs/entity-page.jsx `EntitiesList`.
  // `onNav(key)` selects an entity, where key === "schema.name".
  let { model, onNav }: { model: SchemaModel; onNav: (key: string) => void } = $props();

  // Refs that touch a table (either endpoint), used for the "Refs" column count.
  function refCount(schema: string, name: string): number {
    return model.refs.filter(
      (r) =>
        (r.from.s === schema && r.from.t === name) || (r.to.s === schema && r.to.t === name)
    ).length;
  }

  // The comment is the user's own local DDL comment — trusted input for this
  // offline tool — so rendering it via {@html} is acceptable. marked.parse is
  // synchronous (no async extensions configured); cast to string.
  function noteHtml(md: string | undefined): string {
    return md ? (marked.parse(md) as string) : '';
  }
</script>

<div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-paper">
  <div class="mx-auto max-w-5xl px-6 pb-6">
    <table class="w-full table-fixed border-collapse text-left">
      <colgroup>
        <col style="width:200px" />
        <col style="width:58px" />
        <col style="width:58px" />
        <col />
      </colgroup>
      <thead>
        <tr class="text-xs font-mono uppercase tracking-wider text-ink-soft">
          <th class="ds-th py-3 pr-4 font-medium">Entity</th>
          <th class="ds-th py-3 pr-4 font-medium">Cols</th>
          <th class="ds-th py-3 pr-4 font-medium">Refs</th>
          <th class="ds-th py-3 font-medium">Comment</th>
        </tr>
      </thead>
      <tbody>
        {#each model.tables as t (nodeId(t.schema, t.name))}
          {@const key = nodeId(t.schema, t.name)}
          {@const refs = refCount(t.schema, t.name)}
          <tr
            data-entity-row={key}
            class="group cursor-pointer border-b border-paper-edge align-top hover:bg-paper-mute"
            onclick={() => onNav(key)}
          >
            <td class="py-3.5 pr-4">
              <div class="text-xs font-mono text-ink-soft" style="overflow-wrap:anywhere">
                {t.schema}.
              </div>
              <div
                class="text-sm font-display font-semibold text-ink group-hover:text-primary"
                style="overflow-wrap:anywhere"
              >
                {t.name}
              </div>
            </td>
            <td class="py-3.5 pr-4 text-xs font-mono text-ink-mute">{t.columns.length}</td>
            <td class="py-3.5 pr-4 text-xs font-mono text-ink-mute">{refs || '—'}</td>
            <td class="py-3.5 text-sm">
              {#if t.noteMd}
                <!-- trusted local DDL comment, see noteHtml above -->
                <div class="vw-md max-w-2xl text-sm text-ink-mute [&_code]:font-mono">
                  {@html noteHtml(t.noteMd)}
                </div>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  /* structural-only; colors come from Rokkit tokens in markup */
  .vw-md :global(strong) {
    font-weight: 600;
    color: var(--ink);
  }
  .vw-md :global(p) {
    margin: 0.25rem 0;
  }
  .vw-md :global(code) {
    font-size: 0.85em;
  }
</style>
