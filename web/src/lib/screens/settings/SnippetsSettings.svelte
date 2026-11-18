<script lang="ts">
  import { onMount } from 'svelte';
  import { getRuntime } from '#lib/app/runtime.js';
  import { errorText } from '#lib/api/errors.js';
  import Button from '#lib/ui/Button.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import type { Snippet } from '#lib/snippets/store.svelte.js';

  const runtime = getRuntime();
  const store = runtime.snippets;
  const projectKey = [...runtime.list.items.values()].find((row) => row.workdir)?.workdir ?? '';

  let title = $state('');
  let body = $state('');
  let scope = $state<'global' | 'project'>('global');
  let editing = $state<Snippet | null>(null);
  let deleting = $state<Snippet | null>(null);
  let busy = $state(false);
  let error = $state('');

  onMount(() => {
    store.setProject(projectKey);
    void store.load();
  });

  const items = $derived(store.ordered());

  async function save(): Promise<void> {
    if (!title.trim() || !body.trim()) return;
    busy = true;
    error = '';
    try {
      if (editing) {
        await store.update(editing.id, {
          title: title.trim(),
          body,
          base_revision: editing.revision
        });
      } else {
        await store.create({
          scope,
          project_key: scope === 'project' ? projectKey : '',
          title: title.trim(),
          body
        });
      }
      reset();
    } catch (cause) {
      error = errorText(cause, 'Не удалось сохранить заготовку. Повторите при восстановлении связи');
    } finally {
      busy = false;
    }
  }

  function edit(item: Snippet): void {
    editing = item;
    title = item.title;
    body = item.body;
    scope = item.scope as 'global' | 'project';
    error = '';
  }

  function reset(): void {
    editing = null;
    title = '';
    body = '';
    error = '';
  }

  async function remove(): Promise<void> {
    const item = deleting;
    deleting = null;
    if (!item) return;
    try {
      await store.remove(item.id);
    } catch (cause) {
      error = errorText(cause, 'Не удалось удалить заготовку. Повторите при восстановлении связи');
    }
  }

  async function move(item: Snippet, delta: number): Promise<void> {
    const siblings = items.filter((row) => row.scope === item.scope && row.project_key === item.project_key);
    const from = siblings.findIndex((row) => row.id === item.id);
    const to = from + delta;
    if (from < 0 || to < 0 || to >= siblings.length) return;
    const order = [...siblings];
    const [moved] = order.splice(from, 1);
    order.splice(to, 0, moved!);
    try {
      await store.move(item.scope as 'global' | 'project', item.project_key, order.map((row) => row.id));
    } catch (cause) {
      error = errorText(cause, 'Не удалось изменить порядок. Повторите при восстановлении связи');
    }
  }
</script>

<div class="group">
  <section class="setting">
    <h3>Заготовки сообщений</h3>
    <p class="note">Текст вставляется в поле ввода одним нажатием и не отправляется сам. Проектные заготовки привязаны к рабочей папке текущей сессии</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}

    <div class="editor">
      <label>Название<input bind:value={title} maxlength="80" aria-label="Название заготовки" /></label>
      <label>Текст<textarea bind:value={body} rows="3" aria-label="Текст заготовки"></textarea></label>
      <label>Область
        <select bind:value={scope} disabled={Boolean(editing)} aria-label="Область заготовки">
          <option value="global">Все сессии</option>
          <option value="project">Проект</option>
        </select>
      </label>
      <div class="editor-actions">
        {#if !title.trim() || !body.trim()}
          <Button disabled reason="Нужны название и текст">{editing ? 'Сохранить правки' : 'Добавить заготовку'}</Button>
        {:else if busy}
          <Button disabled reason="Сохраняю…">Сохраняю…</Button>
        {:else}
          <Button onclick={() => void save()}>{editing ? 'Сохранить правки' : 'Добавить заготовку'}</Button>
        {/if}
        {#if editing}<Button onclick={reset}>Отменить правки</Button>{/if}
      </div>
    </div>

    {#if !store.loaded}<p class="note" role="status">Загружаю заготовки…</p>{/if}
    {#if items.length}
      <ul class="list">
        {#each items as item (item.id)}
          <li>
            <div class="row">
              <div class="label">
                <span>{item.title}</span>
                <small>{item.scope === 'project' ? 'Проект' : 'Все сессии'}</small>
              </div>
              <IconButton icon="edit" label={`Изменить: ${item.title}`} onclick={() => edit(item)} />
              <IconButton icon="grip" label={`Выше: ${item.title}`} onclick={() => void move(item, -1)} />
              <IconButton icon="grip" label={`Ниже: ${item.title}`} onclick={() => void move(item, 1)} />
              <IconButton icon="trash" label={`Удалить: ${item.title}`} onclick={() => (deleting = item)} />
            </div>
          </li>
        {/each}
      </ul>
    {:else if store.loaded}
      <p class="note">Заготовок пока нет</p>
    {/if}
  </section>
</div>

<ConfirmDialog
  open={Boolean(deleting)}
  title="Удалить заготовку"
  body={`Удалить «${deleting?.title ?? ''}»?`}
  confirmLabel="Удалить"
  danger
  onconfirm={() => void remove()}
  onclose={() => (deleting = null)}
/>

<style>
  .editor { display: grid; gap: var(--s2); margin-top: var(--s3); }
  .editor label { display: grid; gap: var(--s1); color: var(--secondary); font-size: var(--fs-sm); }
  .editor input, .editor select, .editor textarea {
    min-height: var(--target);
    padding: var(--s1) var(--s2);
    border: var(--hairline) solid var(--line);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
    font: inherit;
  }
  .editor textarea { resize: vertical; }
  .editor-actions { display: flex; gap: var(--s2); flex-wrap: wrap; }
  .list { list-style: none; margin: var(--s3) 0 0; padding: 0; display: grid; gap: var(--s1); }
  .row { display: flex; align-items: center; gap: var(--s2); min-height: var(--target); }
  .label { display: grid; flex: 1; min-width: 0; }
  .label small { color: var(--muted); font-size: var(--fs-xs); }
  .error { color: var(--danger); font-size: var(--fs-sm); }
</style>
