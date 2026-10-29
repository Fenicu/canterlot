<script lang="ts">
  import { createGroup, deleteGroup, patchGroup } from '$lib/api/client';
  import { isTouchPrimary } from '$lib/composer/sendRules';
  import Sheet from '$lib/ui/Sheet.svelte';

  let {
    groups,
    touch = isTouchPrimary(),
    onclose
  }: {
    groups: { id: string; name: string; position: number }[];
    touch?: boolean;
    onclose: () => void;
  } = $props();

  let name = $state('');
  let confirmId = $state('');
  const ordered = $derived([...groups].sort((a, b) => a.position - b.position));

  async function add() {
    const next = name.trim();
    if (!next) return;
    await createGroup({ name: next }, crypto.randomUUID());
    name = '';
  }

  async function rename(id: string, value: string) {
    const next = value.trim();
    if (!next) return;
    await patchGroup(id, { name: next }, crypto.randomUUID());
  }

  async function move(index: number, dir: number) {
    const other = ordered[index + dir];
    const current = ordered[index];
    if (!other || !current) return;
    await patchGroup(current.id, { position: other.position }, crypto.randomUUID());
    await patchGroup(other.id, { position: current.position }, crypto.randomUUID());
  }

  async function remove(id: string) {
    await deleteGroup(id, crypto.randomUUID());
    confirmId = '';
  }
</script>

{#snippet fields()}
  <form onsubmit={(event) => { event.preventDefault(); void add(); }}>
    <label>Новая группа <input bind:value={name} aria-label="Название группы" /></label>
    <button type="submit">Создать</button>
  </form>
  {#each ordered as group, index (group.id)}
    <div>
      <input aria-label={`Название ${group.name}`} value={group.name} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); void rename(group.id, event.currentTarget.value); } }} />
      <button type="button" onclick={() => void move(index, -1)}>Выше</button>
      <button type="button" onclick={() => void move(index, 1)}>Ниже</button>
      <button type="button" onclick={() => (confirmId = group.id)}>Удалить</button>
      {#if confirmId === group.id}
        <p>Сессии останутся в списке</p>
        <button type="button" onclick={() => void remove(group.id)}>Подтвердить удаление</button>
      {/if}
    </div>
  {/each}
{/snippet}

{#if touch}
  <Sheet open title="Группы" {onclose}>{@render fields()}</Sheet>
{:else}
  <dialog open aria-label="Группы">
    <button type="button" onclick={onclose}>Закрыть</button>
    {@render fields()}
  </dialog>
{/if}

<style>
  dialog {
    position: fixed;
    inset: 0;
    margin: auto;
    width: min(100%, var(--phone-max));
    height: fit-content;
    border: var(--s0);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    padding: var(--s6);
  }
  label, div { display: flex; flex-wrap: wrap; gap: var(--s2); align-items: center; }
  input { min-height: var(--target); border: var(--s0); border-radius: var(--radius-sm); background: var(--bg); color: var(--text); }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--accent-link); }
  p { color: var(--secondary); }
</style>
