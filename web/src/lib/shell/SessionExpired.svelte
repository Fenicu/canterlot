<script lang="ts">
  import { session } from '$lib/api/http';
  import type { ConnectionPhase } from '$lib/sync/connection';

  let { phase, now }: { phase: ConnectionPhase; now: number } = $props();
  let dialog = $state<HTMLDialogElement | null>(null);
  const open = $derived(phase === 'auth_required' || (now >= 0 && session.state === 'expired'));

  $effect(() => {
    const node = dialog;
    if (!node) return;
    if (open && !node.open) node.showModal();
    if (!open && node.open) node.close();
  });

  function again() {
    window.location.assign('/');
  }
</script>

<dialog bind:this={dialog} aria-labelledby="expired-title">
  <h2 id="expired-title">Сессия истекла</h2>
  <p>Войдите снова, чтобы продолжить.</p>
  <button type="button" onclick={again}>Войти снова</button>
</dialog>

<style>
  dialog {
    border: var(--s0);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    padding: var(--s8);
    max-width: var(--phone-max);
  }
  dialog::backdrop { background: var(--scrim); }
  h2 { margin: 0; font-size: var(--fs-title); }
  p { color: var(--secondary); }
  button {
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--accent);
    color: var(--accent-ink);
    padding: var(--s4) var(--s8);
  }
</style>
