<script lang="ts">
  import '../lib/styles/tokens.css';
  import '../lib/styles/base.css';
  import '../lib/styles/prose.css';
  import '../lib/styles/highlight.css';
  import { onMount, type Component, type Snippet } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { initTheme } from '#lib/theme.js';
  import { announce, mountAnnouncements } from '#lib/a11y/announce.js';
  import AppShell from '#lib/shell/AppShell.svelte';
  import type { ChangelogEntry } from '#lib/changelog.js';
  import { whatsNew } from '#lib/stores/appVersion.js';
  let { children }: { children: Snippet } = $props();
  let WhatsNewDialog = $state<Component<{ entries: ChangelogEntry[]; onclose: () => void }> | null>(null);

  $effect(() => {
    if ($whatsNew.open && !WhatsNewDialog) {
      import('#lib/screens/changelog/WhatsNewDialog.svelte').then((m) => {
        WhatsNewDialog = m.default;
      });
    }
  });

  onMount(() => {
    initTheme();
    // Первый вход ничего не показывает, но запоминает версию.
    void whatsNew.check();
  });

  afterNavigate(({ shallow, type }) => {
    if (shallow && type === 'goto') return;

    return announce(document.title);
  });
</script>
<a class="skip-link" href="#main">К содержимому</a>
<div id="app-shell"><AppShell>{@render children()}</AppShell></div>
{#if $whatsNew.open && WhatsNewDialog}
  <WhatsNewDialog entries={$whatsNew.entries} onclose={() => whatsNew.dismiss()} />
{/if}
<div class="sr-only" aria-live="polite" aria-atomic="true" use:mountAnnouncements></div>
