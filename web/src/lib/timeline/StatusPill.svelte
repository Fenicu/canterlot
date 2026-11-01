<script lang="ts">
  let { state, text }: { state: 'running' | 'done' | 'error' | 'answer' | 'background'; text: string } = $props();
  // Живая отметка времени только растёт в ширину: цифры равной ширины, а запас держит самую длинную запись, и край подписи рядом не гуляет.
  let reach = 0;
  const reserve = $derived(state === 'running' ? (reach = Math.max(reach, text.length)) : 0);
</script>

<span class="pill" data-state={state} style:--reach={reserve > 0 ? reserve : null}>{text}</span>

<style>
  .pill {
    flex: none;
    margin-left: auto;
    padding: var(--s1) var(--s4);
    border-radius: var(--radius-pill);
    background: var(--chip);
    color: var(--chip-text);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .pill[data-state='done'] { padding-inline: var(--s0); background: var(--transparent); color: var(--time); }
  .pill[data-state='error'] { background: var(--danger-soft); color: var(--danger); }
  .pill[data-state='running'], .pill[data-state='answer'] { background: var(--accent-soft); color: var(--accent-link); }
  .pill[data-state='running'] { min-width: calc(var(--reach, 0) * 1ch + var(--s4) * 2); text-align: center; }
  .pill[data-state='answer'] { flex: 0 1 auto; min-width: 0; max-width: 60%; overflow: hidden; text-overflow: ellipsis; }
</style>
