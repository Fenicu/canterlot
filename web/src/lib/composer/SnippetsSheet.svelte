<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Button from '#lib/ui/Button.svelte';
  import { errorText } from '#lib/api/errors.js';

  let {
    open,
    projectKey,
    text,
    onclose,
    oninsert
  }: {
    open: boolean;
    projectKey: string;
    text: string;
    onclose: () => void;
    oninsert: (body: string) => void;
  } = $props();

  const store = getRuntime().snippets;
  let creating = $state(false);
  let title = $state('');
  let scope = $state<'global' | 'project'>('project');
  let busy = $state(false);
  let error = $state('');

  $effect(() => {
    if (!open) return;
    store.setProject(projectKey);
    void store.load();
    title = '';
    creating = false;
  });

  const items = $derived(store.ordered());
  const projectItems = $derived(items.filter((item) => item.scope === 'project'));
  const globalItems = $derived(items.filter((item) => item.scope === 'global'));

  async function save(): Promise<void> {
    const name = title.trim();
    if (!name || !text.trim()) return;
    busy = true;
    error = '';
    try {
      await store.create({
        scope,
        project_key: scope === 'project' ? projectKey : '',
        title: name,
        body: text
      });
      creating = false;
      title = '';
    } catch (cause) {
      error = errorText(cause, 'Не удалось сохранить заготовку. Повторите при восстановлении связи');
    } finally {
      busy = false;
    }
  }

</script>

<Sheet {open} title="Заготовки" {onclose}>
  {#if !creating}
    <div class="actions">
      {#if !text.trim()}
        <Button disabled reason="Поле ввода пусто">Сохранить как заготовку</Button>
      {:else}
        <Button onclick={() => (creating = true)}>Сохранить как заготовку</Button>
      {/if}
    </div>
  {:else}
    <div class="editor">
      <label>Название<input bind:value={title} maxlength="80" aria-label="Название заготовки" /></label>
      <label>Область
        <select bind:value={scope} aria-label="Область заготовки">
          <option value="project">Проект</option>
          <option value="global">Все сессии</option>
        </select>
      </label>
      <div class="editor-actions">
        {#if busy || !title.trim() || !text.trim()}
          <Button disabled reason={busy ? 'Сохраняю…' : 'Нет названия или текста'}>Сохранить</Button>
        {:else}
          <Button onclick={() => void save()}>Сохранить</Button>
        {/if}
        <Button onclick={() => (creating = false)}>Отменить</Button>
      </div>
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if store.error}<p class="error" role="alert">{store.error}</p>{/if}
  {#if !store.loaded}<p class="note" role="status">Загружаю заготовки…</p>{/if}

  {#if projectItems.length}
    <h3>Проект</h3>
    <ul class="list">
      {#each projectItems as item (item.id)}
        <li><button type="button" class="pick" onclick={() => oninsert(item.body)}>{item.title}</button></li>
      {/each}
    </ul>
  {/if}
  {#if globalItems.length}
    <h3>Все сессии</h3>
    <ul class="list">
      {#each globalItems as item (item.id)}
        <li><button type="button" class="pick" onclick={() => oninsert(item.body)}>{item.title}</button></li>
      {/each}
    </ul>
  {/if}
  {#if store.loaded && items.length === 0}<p class="note">Заготовок пока нет</p>{/if}
</Sheet>

<style>
  .actions, .editor-actions { display: flex; gap: var(--s2); flex-wrap: wrap; }
  .editor { display: grid; gap: var(--s2); }
  .editor label { display: grid; gap: var(--s1); color: var(--secondary); font-size: var(--fs-sm); }
  .editor input, .editor select {
    min-height: var(--target);
    padding: var(--s1) var(--s2);
    border: var(--hairline) solid var(--line);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
    font: inherit;
  }
  h3 { margin: var(--s3) 0 var(--s1); color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: var(--s1); }
  .pick {
    width: 100%;
    min-height: var(--target);
    padding: var(--s2);
    text-align: left;
    border: var(--hairline) solid var(--line);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
  }
  .note { color: var(--muted); font-size: var(--fs-sm); }
  .error { color: var(--danger); font-size: var(--fs-sm); }
</style>
