<script lang="ts">
  import { createGroup, deleteGroup, patchGroup } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { intents } from '#lib/ops/intent.js';
  import { fmt } from '#lib/format/ru.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

  let {
    groups,
    counts = {},
    onclose
  }: {
    groups: { id: string; name: string; position: number }[];
    counts?: Record<string, number>;
    onclose: () => void;
  } = $props();

  let name = $state('');
  let confirmId = $state('');
  const ordered = $derived([...groups].sort((a, b) => a.position - b.position));
  const sessions = (id: string) => {
    const count = counts[id] ?? 0;
    return `${count} ${fmt.plural(count, ['сессия', 'сессии', 'сессий'])}`;
  };

  let error = $state('');
  const creating = $derived(intents.busy(`group-create:${name.trim()}`));

  async function guarded(fallback: string, call: () => Promise<unknown>): Promise<boolean> {
    error = '';
    try {
      await call();
      return true;
    } catch (err) {
      error = errorText(err, fallback);
      return false;
    }
  }

  async function add() {
    const next = name.trim();
    if (!next) return;
    if (await guarded('Не удалось создать группу', () => intents.run(`group-create:${next}`, (key) => createGroup({ name: next }, key)))) name = '';
  }

  async function rename(input: HTMLInputElement, id: string, previous: string) {
    const next = input.value.trim();
    if (!next || next === previous) return;
    const body = { name: next };
    const ok = await guarded('Не удалось переименовать группу', () => intents.run(`group-patch:${id}:${JSON.stringify(body)}`, (key) => patchGroup(id, body, key)));
    if (!ok) input.value = previous;
  }

  async function move(index: number, dir: number) {
    const other = ordered[index + dir];
    const current = ordered[index];
    if (!other || !current) return;
    const place = (id: string, position: number) => intents.run(`group-patch:${id}:${position}`, (key) => patchGroup(id, { position }, key));
    await guarded('Не удалось изменить порядок', async () => {
      await place(current.id, other.position);
      try {
        await place(other.id, current.position);
      } catch (err) {
        // Вторая половина обмена не дошла: первую возвращаем, иначе у двух групп одна позиция.
        await place(current.id, current.position).catch(() => undefined);
        throw err;
      }
    });
  }

  async function remove(id: string) {
    if (await guarded('Не удалось удалить группу', () => intents.run(`group-delete:${id}`, (key) => deleteGroup(id, key)))) confirmId = '';
  }
</script>

<Sheet open title="Группы" subtitle="Порядок здесь совпадает с порядком в списке сессий." {onclose}>
  <div class="manager">
    {#each ordered as group, index (group.id)}
      <div class="group">
        <input aria-label={`Название ${group.name}`} value={group.name} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); void rename(event.currentTarget, group.id, group.name); } }} onblur={(event) => void rename(event.currentTarget, group.id, group.name)} />
        <div class="controls">
          <span class="count">{sessions(group.id)}</span>
          <IconButton filled icon="up" label="Выше" inert={index === 0} onclick={() => void move(index, -1)} />
          <IconButton filled icon="down" label="Ниже" inert={index === ordered.length - 1} onclick={() => void move(index, 1)} />
          <IconButton filled icon="trash" label="Удалить" onclick={() => (confirmId = group.id)} />
        </div>
        {#if confirmId === group.id}
          <div class="confirm" role="alert">
            <p>Удалить группу «{group.name}»? Сессии останутся в списке</p>
            <div class="actions">
              <span class="danger"><Button variant="primary" onclick={() => void remove(group.id)}>Подтвердить удаление</Button></span>
              <Button onclick={() => (confirmId = '')}>Отмена</Button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
    <p class="note">Название сохраняется по Enter или когда фокус уходит из поля.</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <form onsubmit={(event) => { event.preventDefault(); void add(); }}>
      <Field label="Новая группа">
        {#snippet children({ id })}<input {id} bind:value={name} aria-label="Название группы" placeholder="Название группы" />{/snippet}
      </Field>
      {#if creating}
        <Button disabled={true} reason="Группа создаётся">Создаю…</Button>
      {:else}
        <Button type="submit">Создать</Button>
      {/if}
    </form>
    <div class="done"><Button variant="primary" onclick={onclose}>Готово</Button></div>
  </div>
</Sheet>

<style>
  .manager { display: flex; flex-direction: column; gap: var(--s4); }
  .group { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s3); padding: var(--s3); border-radius: var(--radius-card); background: var(--raised); }
  .group > input { flex: 1 1 calc(var(--gauge-width) * 2); min-width: 0; min-height: var(--target); padding: var(--s4) var(--s8); border: var(--s0); border-radius: var(--radius-pill); background: var(--chat-bg); color: var(--text); font-size: var(--fs-input); }
  .controls { display: flex; flex: 1 1 auto; align-items: center; justify-content: flex-end; gap: var(--s2); }
  .count { flex: 1; color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .confirm { display: flex; flex: 1 1 100%; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s4); padding: var(--s4) var(--s6); border-radius: var(--radius-card); background: var(--danger-soft); }
  .confirm p { margin: 0; flex: 1 1 calc(var(--gauge-width) * 2); overflow-wrap: anywhere; font-size: var(--fs-meta); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
  .danger { --accent: var(--danger); --accent-ink: var(--bg); display: contents; }
  .note { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
  .error { margin: 0; color: var(--danger); font-size: var(--fs-sm); }
  form { display: flex; align-items: flex-end; gap: var(--s3); }
  form > :global(.field) { flex: 1; }
  .done { display: flex; justify-content: flex-end; }
</style>
