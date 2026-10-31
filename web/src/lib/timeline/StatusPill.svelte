<script lang="ts">
  let { state, text }: { state: 'running' | 'done' | 'error' | 'answer'; text: string } = $props();
</script>

<span class="pill" data-state={state}>{text}</span>

<style>
  .pill {
    flex: none;
    margin-left: auto;
    padding: var(--s1) var(--s4);
    border-radius: var(--radius-pill);
    background: var(--chip);
    color: var(--chip-text);
    font-size: var(--fs-sm);
    white-space: nowrap;
  }
  .pill[data-state='done'] { padding-inline: var(--s0); background: var(--transparent); color: var(--time); }
  .pill[data-state='error'] { background: var(--danger-soft); color: var(--danger); }
  .pill[data-state='running'], .pill[data-state='answer'] { background: var(--accent-soft); color: var(--accent-link); }
  .pill[data-state='answer'] { flex: 0 1 auto; min-width: 0; max-width: 60%; overflow: hidden; text-overflow: ellipsis; }
</style>
