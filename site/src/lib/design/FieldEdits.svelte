<script lang="ts">
  /**
   * The column, constraint, index and enum-value edits inside one changed entity — rendered the
   * same wherever a changelog lists them: the project's (#29) and an entity's own (#33).
   */
  import { OP_CLASS, OP_MARK } from './changelog';
  import type { FieldEdit } from './model';

  let { fields }: { fields: FieldEdit[] } = $props();

  /** The definition side of an edit, as one line: `from → to`, or whichever side exists. */
  function detail(f: FieldEdit): string {
    if (f.op === 'renamed') return `${f.from} → ${f.to}`;
    if (f.from && f.to) return `${f.from} → ${f.to}`;
    return f.to ?? f.from ?? f.note ?? '';
  }
</script>

<ul class="flex flex-col gap-1">
  {#each fields as f, i (i)}
    <li data-field-op={f.op} class="flex flex-wrap items-baseline gap-2 font-mono text-xs text-muted">
      <span class="w-3 {OP_CLASS[f.op]}">{OP_MARK[f.op]}</span>
      <span class="text-faint">{f.kind}</span>
      <!-- A rename is its own `old → new`; naming the new one first says it twice. -->
      {#if f.op !== 'renamed'}<span class={f.op === 'removed' ? 'text-faint line-through' : 'text-fg'}>{f.name}</span
        >{/if}
      {#if detail(f)}<span class={f.op === 'renamed' ? 'text-fg' : ''} style="overflow-wrap: anywhere;"
          >{detail(f)}</span
        >{/if}
    </li>
  {/each}
</ul>
