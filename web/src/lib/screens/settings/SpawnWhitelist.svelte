<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { getSpawnWhitelist, listMachines, putSpawnWhitelist } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import Button from '#lib/ui/Button.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import Select from '#lib/ui/Select.svelte';
  import {
    NOT_IN_CATALOG, addEntry, describeEntries, harnessOptions, mergeCatalog, modelOptions,
    removeEntry, sameEntries, type CatalogHarness, type WhitelistEntry, type WhitelistView
  } from './spawnWhitelist.js';

  let view = $state<WhitelistView | null>(null);
  let draft = $state<WhitelistEntry[]>([]);
  let catalog = $state<CatalogHarness[]>([]);
  let harness = $state('');
  let model = $state('default');
  let error = $state('');
  let addError = $state('');
  let busy = $state(false);

  const rows = $derived(describeEntries(draft, catalog));
  const dirty = $derived(view !== null && !sameEntries(draft, view.entries));
  const harnesses = $derived(harnessOptions(catalog));
  const models = $derived(modelOptions(catalog, harness));
  const canReset = $derived(view !== null && (!view.is_default || dirty));

  $effect(() => { if (!harnesses.some((item) => item.value === harness)) harness = harnesses[0]?.value ?? ''; });
  $effect(() => { if (!models.some((item) => item.value === model)) model = 'default'; });

  function accept(next: WhitelistView) { view = next; draft = [...next.entries]; }
  async function load() {
    error = '';
    try {
      const [list, machines] = await Promise.all([getSpawnWhitelist(), listMachines()]);
      catalog = mergeCatalog(machines.data);
      accept(list.data);
    } catch { error = 'Не удалось загрузить список делегирования. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  function add() {
    addError = '';
    const result = addEntry(draft, { harness, model });
    if (result.ok) draft = result.entries; else addError = result.error;
  }
  async function send(entries: WhitelistEntry[] | null) {
    if (busy) return;
    busy = true; error = '';
    try { accept((await putSpawnWhitelist({ entries }, crypto.randomUUID())).data); }
    catch (err) { error = err instanceof ApiError ? err.message : 'Не удалось сохранить список. Повторите при восстановлении связи'; }
    finally { busy = false; }
  }
  function reset() {
    addError = '';
    if (view?.is_default) draft = [...view.entries]; else void send(null);
  }
</script>

<section data-spawn-whitelist>
  <h2>Делегирование</h2>
  <p class="note">Сессии, которые агент запускает без вопроса</p>
  {#if error}<p class="error" role="alert">{error}</p>{#if !view}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if view}
    <ul>
      {#each rows as row (row.harness + '/' + row.model)}
        <li>
          <span class="name">{row.harnessTitle}<span class="model">{row.modelTitle}</span></span>
          {#if !row.known}<Pill color="amber">{NOT_IN_CATALOG}</Pill>{/if}
          <button type="button" aria-label={`Убрать ${row.harnessTitle}, ${row.modelTitle}`} onclick={() => { addError = ''; draft = removeEntry(draft, row); }}>Убрать</button>
        </li>
      {/each}
    </ul>
    <p class="note">Остальное агент запрашивает карточкой</p>
    <div class="add">
      <Select label="Агент" options={harnesses} bind:value={harness} disabled={harnesses.length === 0} />
      <Select label="Модель" options={models} bind:value={model} disabled={harnesses.length === 0} />
      {#if harnesses.length === 0}<Button disabled reason="Нет машин с каталогом моделей">Добавить</Button>{:else}<Button onclick={add}>Добавить</Button>{/if}
    </div>
    {#if addError}<p class="error" role="alert">{addError}</p>{/if}
    <div class="actions">
      {#if dirty && !busy}<Button variant="primary" onclick={() => send(draft)}>Сохранить</Button>{:else}<Button variant="primary" disabled reason={busy ? 'Сохраняю…' : 'Нет изменений'}>Сохранить</Button>{/if}
      {#if canReset && !busy}<Button onclick={reset}>Сбросить к списку по умолчанию</Button>{:else}<Button disabled reason={busy ? 'Сохраняю…' : 'Уже стоит список по умолчанию'}>Сбросить к списку по умолчанию</Button>{/if}
    </div>
  {:else if !error}
    <p>Загружаю список делегирования…</p>
  {/if}
  <p class="routing"><a href={resolve('/settings/routing')} data-role="routing-link">Маршрутизация</a><span class="note">Кого брать для каждого типа задачи, пороги лимитов и проверка выбора</span></p>
</section>

<style>
  section { margin-bottom: var(--s10); }
  ul { list-style: none; margin: var(--s4) 0; padding: var(--s0); display: grid; gap: var(--s3); }
  li { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); padding: var(--s4) var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  .name { flex: 1 1 var(--s0); min-width: var(--s0); display: grid; }
  .model { color: var(--muted); font-size: var(--fs-meta); }
  .add { display: grid; gap: var(--s4); margin-block: var(--s4); }
  .actions { display: grid; gap: var(--s4); }
  .actions :global(.reason) { margin-block: var(--s0); }
  .routing { display: grid; gap: var(--s1); margin-top: var(--s8); }
  .routing a { display: inline-flex; align-items: center; min-height: var(--target); }
</style>
