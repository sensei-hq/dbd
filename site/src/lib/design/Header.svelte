<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import type { HeaderData } from './data';
  // Inlined trusted local SVG (Vite resolves `?raw` to the file's text). Lives
  // in src/ (not static/) — Vite dev disallows importing public-dir assets.
  import dbdLogo from '$lib/assets/dbd.svg?raw';

  let {
    data,
    theme = 'light',
    brandHref,
    showUser = true,
    onBrand,
    onCrumb,
    onToggleTheme,
    onShare,
  }: {
    data: HeaderData;
    theme?: 'light' | 'dark';
    /** When set, the brand renders as a link to this URL; otherwise a button. */
    brandHref?: string;
    /** Hide the user avatar when there's no signed-in user yet. */
    showUser?: boolean;
    onBrand?: () => void;
    /** Clicked an intermediate crumb (no href, not current) — e.g. project name. */
    onCrumb?: (index: number) => void;
    onToggleTheme?: () => void;
    onShare?: () => void;
  } = $props();
</script>

{#snippet brand()}
  <!-- eslint-disable-next-line svelte/no-at-html-tags — inlined trusted local SVG asset -->
  <span class="hdr-logo" aria-hidden="true">{@html dbdLogo}</span>
  <span class="font-display text-base font-semibold tracking-tight">{data.brand.name}</span>
  <span class="ds-badge" style="transform: translateY(1px);">{data.brand.badge}</span>
{/snippet}

<header
  class="flex items-center gap-3 border-b border-line bg-paper px-4 lg:px-6"
  style="height: var(--header-h); flex: 0 0 auto;"
>
  {#if brandHref}
    <a href={brandHref} class="hdr-brand flex items-center gap-2.5" title="Your designs">{@render brand()}</a>
  {:else}
    <button type="button" class="hdr-brand flex items-center gap-2.5" title="Your designs" onclick={onBrand}>
      {@render brand()}
    </button>
  {/if}

  {#if data.crumbs.length}
    <nav class="ml-2 hidden items-center gap-1.5 text-sm text-muted sm:flex" aria-label="Breadcrumb">
      {#each data.crumbs as c, i (c.label)}
        <span class="text-faint">/</span>
        {#if c.current}
          <span class="font-medium text-fg">{c.label}</span>
        {:else if c.href}
          <a class="hover:text-fg" href={c.href}>{c.label}</a>
        {:else}
          <button type="button" class="hover:text-fg" onclick={() => onCrumb?.(i)}>{c.label}</button>
        {/if}
      {/each}
    </nav>
  {/if}

  <div class="ml-auto flex items-center gap-2">
    <button type="button" class="ds-btn" onclick={onShare}>
      <Icon name="link" size={14} />
      Share
    </button>
    <button
      type="button"
      class="ds-iconbtn"
      title={theme === 'light' ? 'Switch to dark' : 'Switch to light'}
      onclick={onToggleTheme}
    >
      <Icon name={theme === 'light' ? 'moon' : 'sun'} size={17} />
    </button>
    {#if showUser && data.user}
      <div class="hdr-avatar font-display font-semibold select-none" title={data.user.email}>
        {data.user.initials}
      </div>
    {/if}
  </div>
</header>

<style>
  .hdr-brand {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: inherit;
    font: inherit;
  }
  .hdr-logo {
    display: inline-flex;
    width: 26px;
    height: 26px;
    flex: 0 0 auto;
    color: var(--accent);
  }
  .hdr-logo :global(svg) {
    width: 100%;
    height: 100%;
    display: block;
  }
  .hdr-avatar {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    font-size: 11.5px;
    border-radius: 9999px;
    background: var(--accent-soft);
    color: var(--accent-2);
    border: 1px solid var(--accent-line);
  }
</style>
