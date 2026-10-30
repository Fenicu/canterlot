<script lang="ts">
  import { resolve } from '$app/paths';
  import TopBar from '#lib/ui/TopBar.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { theme } from '#lib/theme.js';
  let { title, subtitle }: { title: string; subtitle?: string } = $props();
</script>
<TopBar {title} {subtitle}>
  {#snippet leading()}<a href={resolve('/sessions')} aria-label="К сессиям"><Icon name="back" /></a>{/snippet}
  {#snippet actions()}<IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}
</TopBar>
<style>a { min-height: var(--target); min-width: var(--target); justify-content: center; display: flex; align-items: center; padding: var(--s4); color: var(--text); }</style>
