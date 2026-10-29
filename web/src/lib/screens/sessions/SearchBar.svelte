<script lang="ts">
  let { value = $bindable(''), onreset }: { value?: string; onreset: () => void } = $props();
  let field: HTMLInputElement | undefined = $state();

  $effect(() => {
    field?.focus();
  });

  function keydown(event: KeyboardEvent) {
    if (event.key !== 'Escape') return;
    event.preventDefault();
    value = '';
    onreset();
  }
</script>

<div class="search">
  <label>
    <span class="sr-only">Поиск сессий</span>
    <input bind:this={field} bind:value aria-label="Поиск сессий" onkeydown={keydown} />
  </label>
  <button type="button" onclick={onreset}>Сбросить</button>
</div>

<style>
  .search { display: flex; gap: var(--s2); margin: var(--s3) var(--s6); }
  label { flex: 1; }
  input {
    width: 100%;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--text);
    padding: var(--s4) var(--s6);
  }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
