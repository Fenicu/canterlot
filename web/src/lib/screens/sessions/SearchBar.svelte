<script lang="ts">
  let { value = $bindable(''), onreset }: { value?: string; onreset: () => void } = $props();
  let field: HTMLInputElement | undefined = $state();

  $effect(() => {
    field?.focus();
  });

  function reset() {
    onreset();
    field?.focus();
  }

  function keydown(event: KeyboardEvent) {
    if (event.key !== 'Escape') return;
    event.preventDefault();
    value = '';
    onreset();
  }
</script>

<div class="search" role="search">
  <label>
    <span class="sr-only">Поиск сессий</span>
    <input bind:this={field} bind:value aria-label="Поиск сессий" placeholder="Поиск сессий" onkeydown={keydown} />
  </label>
  {#if value}<button type="button" onclick={reset}>Сбросить</button>{/if}
</div>

<style>
  .search { display: flex; align-items: center; gap: var(--s2); margin: var(--s3) var(--s6) var(--s4); border-radius: var(--radius-pill); background: var(--raised); }
  /* Кольцо фокуса обводит всю капсулу вместе с «Сбросить», а не поле внутри неё. */
  .search:has(input:focus-visible) { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  input:focus-visible { outline: none; }
  label { flex: 1; min-width: 0; }
  input {
    width: 100%;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--transparent);
    color: var(--text);
    padding: var(--s4) var(--s6);
  }
  button { flex-shrink: 0; min-height: var(--target); padding-inline: var(--s6); border: var(--s0); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-meta); }
</style>
