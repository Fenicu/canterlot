<script lang="ts">
  import '../lib/styles/tokens.css';
  import '../lib/styles/base.css';
  import '../lib/styles/prose.css';
  import '../lib/styles/highlight.css';
  import { onMount, type Snippet } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { initTheme } from '#lib/theme.js';
  import { announce, mountAnnouncements } from '#lib/a11y/announce.js';
  import AppShell from '#lib/shell/AppShell.svelte';
  let { children }: { children: Snippet } = $props();
  onMount(initTheme);

  afterNavigate(({ shallow, type }) => {
    if (shallow && type === 'goto') return;

    return announce(document.title);
  });
</script>
<a class="skip-link" href="#main">К содержимому</a>
<div id="app-shell"><AppShell>{@render children()}</AppShell></div>
<div class="sr-only" aria-live="polite" aria-atomic="true" use:mountAnnouncements></div>
