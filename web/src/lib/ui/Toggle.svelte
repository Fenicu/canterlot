<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';
  let { checked = $bindable(false), label, hint, icon, block = false, hideLabel = false }: { checked?: boolean; label: string; hint?: string; icon?: IconName; block?: boolean; hideLabel?: boolean } = $props();
</script>
<label class:block>
  {#if icon}<Icon name={icon} />{/if}
  <span class="text" class:sr-only={hideLabel}><span>{label}</span>{#if hint}<small>{hint}</small>{/if}</span>
  <span class="toggle"><input type="checkbox" role="switch" bind:checked /><span class="toggle-track"></span></span>
</label>
<style>
  label { display: inline-flex; min-height: var(--target); align-items: center; gap: var(--s6); cursor: pointer; }
  .block { display: flex; width: 100%; }
  .block .text { flex: 1; min-width: 0; }
  .text { display: flex; flex-direction: column; gap: var(--s1); }
  .toggle { position: relative; display: inline-flex; width: var(--toggle-width); height: var(--toggle-height); flex-shrink: 0; }
  input { position: absolute; opacity: 0; width: 100%; height: 100%; margin: 0; cursor: pointer; }
  /* Выключенный: светлая ручка на приглушённой дорожке, включённый: акцент. Положение ручки не единственный признак. */
  .toggle-track { width: 100%; height: 100%; border-radius: var(--radius-pill); background: var(--control-line); padding: var(--s2); pointer-events: none; }
  .toggle-track::after { content: ''; display: block; width: var(--toggle-knob); height: var(--toggle-knob); border-radius: var(--radius-round); background: var(--surface); }
  input:checked + .toggle-track { background: var(--accent); }
  input:checked + .toggle-track::after { margin-left: auto; background: var(--accent-ink); }
  input:focus-visible + .toggle-track { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
</style>
