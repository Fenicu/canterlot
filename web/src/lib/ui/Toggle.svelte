<script lang="ts">
  let { checked = $bindable(false), label }: { checked?: boolean; label: string } = $props();
</script>
<label><span>{label}</span><span class="toggle"><input type="checkbox" role="switch" bind:checked /><span class="toggle-track"></span></span></label>
<style>
  label { display: inline-flex; min-height: var(--target); align-items: center; gap: var(--s6); cursor: pointer; }
  .toggle { position: relative; display: inline-flex; width: var(--toggle-width); height: var(--toggle-height); flex-shrink: 0; }
  input { position: absolute; opacity: 0; width: 100%; height: 100%; margin: 0; cursor: pointer; }
  .toggle-track { width: 100%; height: 100%; border-radius: var(--radius-pill); background: var(--line); padding: var(--s2); pointer-events: none; }
  .toggle-track::after { content: ''; display: block; width: var(--toggle-knob); height: var(--toggle-knob); border-radius: var(--radius-round); background: var(--text); }
  input:checked + .toggle-track { background: var(--accent); }
  input:checked + .toggle-track::after { margin-left: auto; background: var(--accent-ink); }
  input:focus-visible + .toggle-track { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
</style>
