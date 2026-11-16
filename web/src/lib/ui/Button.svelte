<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { MouseEventHandler } from 'svelte/elements';
  type Props = { variant?: 'default' | 'primary' | 'text'; full?: boolean; busy?: boolean; type?: 'button' | 'submit'; label?: string; onclick?: MouseEventHandler<HTMLButtonElement>; children: Snippet } & ({ disabled?: false; reason?: never } | { disabled: true; reason: string });
  let { variant = 'default', full = false, busy, type = 'button', label, onclick, children, disabled = false, reason }: Props = $props();
  const id = $props.id();
</script>
<button {type} {onclick} {disabled} aria-label={label} aria-busy={busy} class:primary={variant === 'primary'} class:text-button={variant === 'text'} class:full aria-describedby={disabled ? id : undefined}>{@render children()}</button>
{#if disabled}<p id={id} class="reason">{reason}</p>{/if}
<style>
  button { display: inline-flex; align-items: center; justify-content: center; gap: var(--s4); min-height: var(--target); min-width: var(--target); padding: var(--s5) var(--s8); border: var(--s0); border-radius: var(--radius-pill); background: var(--button-bg, var(--raised)); font-size: var(--fs-meta); font-weight: var(--weight-medium); text-align: center; }
  .primary, .primary:hover { background: var(--accent); color: var(--accent-ink); }
  .text-button { background: var(--transparent); color: var(--secondary); }
  .full { width: 100%; }
  button:disabled { color: var(--muted); cursor: default; background: var(--button-bg, var(--raised)); }
  .primary:disabled, .primary:disabled:hover { color: var(--muted); background: var(--accent-soft); }
  .text-button:disabled { background: var(--transparent); }
  .reason { color: var(--muted); font-size: var(--fs-sm); margin-top: var(--s2); }
</style>
