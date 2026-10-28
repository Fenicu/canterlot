<script lang="ts">
  import '../lib/styles/tokens.css';
  import '../lib/styles/base.css';
  import { onMount, type Snippet } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { initTheme } from '$lib/theme';
  import { announce, mountAnnouncements } from '$lib/a11y/announce';
  let { children }: { children: Snippet } = $props();
  onMount(initTheme);
  afterNavigate(() => announce(document.title));
</script>
<a class="skip-link" href="#main">К содержимому</a>
<div id="app-shell">{@render children()}</div>
<div class="sr-only" aria-live="polite" aria-atomic="true" use:mountAnnouncements></div>
