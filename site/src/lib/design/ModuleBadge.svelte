<script lang="ts">
  /**
   * The module declaring an entity, or whose version a changelog entry is (#7) — nothing for
   * a project's model, which has no modules.
   */
  let { module }: { module?: string } = $props();
</script>

{#if module}
  <span data-module={module} data-module-badge class="ds-badge" title="Module">{module}</span>
{/if}
