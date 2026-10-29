<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import IconButton from './IconButton.svelte';
  let { open, title, routeKey, onclose, children }: { open: boolean; title: string; routeKey?: string; onclose: () => void; children: Snippet } = $props();
  const id = $props.id();
  let dialog: HTMLDialogElement;
  let mounted = $state(false);
  let opener: HTMLElement | null = null;
  let shell: HTMLElement | null = null;
  let previousInert = false;
  let entry = $state(false);
  function restore() {
    if (shell) shell.inert = previousInert;
    opener?.focus({ preventScroll: true });
  }
  function close() {
    if (!dialog.open) return;
    dialog.close();
    restore();
    if (entry && routeKey && page.state.sheet === routeKey) history.back();
    entry = false;
    onclose();
  }
  function keydown(event: KeyboardEvent) {
    if (event.key === 'Escape') { event.preventDefault(); close(); }
    if (event.key !== 'Tab') return;

    const items = [
      ...dialog.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')
    ].filter((node) => node.getClientRects().length > 0);

    const first = items[0];
    const last = items.at(-1);

    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  }
  onMount(() => { mounted = true; });
  onDestroy(() => { if (dialog?.open) restore(); });
  $effect(() => {
    if (!mounted) return;
    if (open && !dialog.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      shell = document.getElementById('app-shell');
      previousInert = shell?.inert ?? false;

      if (routeKey && page.state.sheet !== routeKey) {
        // eslint-disable-next-line svelte/no-navigation-without-resolve
        void goto('', { shallow: true, state: { ...page.state, sheet: routeKey } }).then(() => {
          entry = true;
        });
      }

      dialog.showModal();
      if (shell) shell.inert = true;
    } else if (!open && dialog.open) close();
  });
  $effect(() => { if (mounted && entry && routeKey && page.state.sheet !== routeKey) close(); });
</script>
<dialog bind:this={dialog} aria-labelledby={id} aria-modal="true" onkeydown={keydown} oncancel={(event) => { event.preventDefault(); close(); }}>
  <div class="handle" aria-hidden="true"></div>
  <header><h2 id={id}>{title}</h2><IconButton icon="close" label="Закрыть" onclick={close} /></header>
  <div class="body">{@render children()}</div>
</dialog>
<style>
  dialog { position: fixed; inset: auto 0 0; width: min(100%, var(--reading-max)); max-width: 100%; max-height: var(--sheet-max); margin: auto auto 0; padding: var(--s4) var(--s8) calc(var(--s8) + env(safe-area-inset-bottom)); border: 0; border-radius: var(--radius-sheet) var(--radius-sheet) 0 0; background: var(--surface); color: var(--text); overflow-y: auto; }
  dialog::backdrop { background: var(--scrim); }
  dialog[open] { animation: sheet-enter var(--duration-expand) var(--ease-sheet); }
  .handle { width: var(--handle-width); height: var(--handle-height); border-radius: var(--radius-pill); background: var(--control-line); margin: var(--s4) auto; }
  header { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); }
  .body { padding-block: var(--s6); }
  @keyframes sheet-enter { from { opacity: 0; transform: translateY(var(--shift-enter)); } to { opacity: 1; transform: none; } }
</style>
