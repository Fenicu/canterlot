<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import Select from '#lib/ui/Select.svelte';
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { getMemory, listMemory } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import Button from '#lib/ui/Button.svelte';
  import EmptyMark from '#lib/ui/EmptyMark.svelte';
  import { rowFoldWhen } from '#lib/ui/fold.js';
  import Segmented from '#lib/ui/Segmented.svelte';
  import ClaudeAutoMemorySetting from './ClaudeAutoMemorySetting.svelte';
  import MemoryEntrySheet from './MemoryEntrySheet.svelte';
  import { DOMAINS, changedLine, collectProjects, entryStatus, kindLabel, kindOptions, levelLabel, projectOptions, resolveLinkedEntry, type MemoryDomain, type MemoryEntry } from './memory.js';

  let domain = $state<MemoryDomain>('work');
  let view = $state<'active' | 'archived'>('active');
  let query = $state('');
  let project = $state('');
  let kind = $state('');
  // Проекты, которые встретились в домене: список фильтра не сужается вместе с выборкой.
  const seenProjects = new SvelteMap<string, string>();
  let items = $state<MemoryEntry[]>([]);
  let next = $state<string | null>(null);
  let loading = $state(true);
  let error = $state('');
  let open = $state<MemoryEntry | null>(null);
  let creating = $state(false);
  // Ответ прежнего домена или фильтра не должен попасть в список нового.
  let generation = 0;

  async function load(more = false) {
    const mine = ++generation;
    loading = true; error = '';
    const cursor = more ? next : null;
    try {
      const page = (await listMemory({ domain, status: view, q: query.trim() || undefined, project: project || undefined, kind: kind || undefined, cursor: cursor ?? undefined })).data;
      if (mine !== generation) return;
      collectProjects(seenProjects, page.items);
      items = more ? [...items, ...page.items] : page.items;
      next = page.next_cursor ?? null;
    } catch (err) {
      if (mine !== generation) return;
      error = errorText(err, 'Не удалось загрузить память. Повторите при восстановлении связи');
    } finally { if (mine === generation) loading = false; }
  }

  // Складываются только строки, что пришли или ушли после правки владельца (новая запись, архив, возврат):
  // смена домена, вида или фильтра и подгрузка страницы строят список сразу.
  let folding = false;
  const fold = rowFoldWhen(() => folding);
  async function reload() {
    folding = true;
    try {
      await load();
      await tick();
    } finally {
      folding = false;
    }
  }

  function pick(value: string, field: 'domain' | 'view') {
    if (field === 'domain') {
      domain = value === 'personal' ? 'personal' : 'work';
      project = ''; kind = ''; seenProjects.clear();
    } else view = value === 'archived' ? 'archived' : 'active';
    items = []; next = null;
    void load();
  }

  function filter(field: 'project' | 'kind', value: string) {
    if (field === 'project') project = value; else kind = value;
    items = []; next = null;
    void load();
  }

  onMount(() => { void load(); });

  // Ссылка из карточки ленты: `?entry=<id>` открывает запись, `&domain=` подсказывает её домен.
  let notice = $state('');
  const linked = $derived(page.url.searchParams.get('entry'));
  $effect(() => {
    const id = linked;
    const hint = page.url.searchParams.get('domain');
    if (id) untrack(() => void openLinked(id, hint));
  });

  async function openLinked(id: string, hint: string | null) {
    notice = '';
    const found = await resolveLinkedEntry(async (entryId, entryDomain) => (await getMemory(entryId, entryDomain)).data, id, hint);
    if (found.domain && found.domain !== domain) pick(found.domain, 'domain');
    if (found.kind === 'notice') { notice = found.notice; return; }
    open = found.entry;
  }
  const excerpt = (text: string) => (text.length > 200 ? `${text.slice(0, 200)}…` : text);
</script>

<section data-memory class="memory" aria-label="Память">
  <div class="head">
    <h2>Память</h2>
    <Button variant="primary" onclick={() => creating = true}>Новая запись</Button>
  </div>
  <p class="note">Общая память агентов: сессии читают и пишут её через инструменты canterlot. Личное и Работа не смешиваются, сессия видит только свой домен</p>

  <Segmented name="memory-domain" label="Домен памяти" options={DOMAINS} bind:value={() => domain, (value) => pick(value, 'domain')} />
  <div class="tools">
    <Segmented name="memory-view" label="Показать записи" options={[{ value: 'active', label: 'Активные' }, { value: 'archived', label: 'В архиве' }]} bind:value={() => view, (value) => pick(value, 'view')} />
    <div class="filters">
      <Select label="Проект" options={projectOptions(seenProjects)} bind:value={() => project, (value) => filter('project', value)} />
      <Select label="Вид записей" options={kindOptions()} bind:value={() => kind, (value) => filter('kind', value)} />
    </div>
    <form class="search" role="search" onsubmit={(event) => { event.preventDefault(); items = []; void load(); }}>
      <input type="search" aria-label="Поиск по памяти" placeholder="Поиск по теме и тексту" bind:value={query} spellcheck="false" />
    </form>
  </div>

  {#if notice}<p class="note" role="status" data-role="memory-link-notice">{notice}</p>{/if}
  {#if error}<p class="error" role="alert">{error}<Button onclick={() => load()}>Повторить</Button></p>{/if}
  {#if !loading && !error && items.length === 0}
    <!-- Значок стоит в строке с текстом: место под списком не растёт. -->
    <p class="empty marked" data-role="memory-empty"><EmptyMark inline name={view === 'archived' ? 'archive' : 'brain'} />{view === 'archived' ? 'В архиве записей нет' : 'Записей пока нет'}</p>
  {/if}
  <ul class="list" data-memory-list={domain} aria-busy={loading}>
    {#each items as item (item.id)}
      {@const status = entryStatus(item)}
      <li transition:fold>
        <button type="button" class="row" data-memory-row={item.id} onclick={() => open = item}>
          <strong>{item.subject}</strong>
          <span class="meta">{kindLabel(item.kind)} · {levelLabel(item)} · версия {item.version}</span>
          <span class="meta" data-role="memory-changed">{changedLine(item)}</span>
          <span class="excerpt">{excerpt(item.body)}</span>
          <span class="status" data-tone={status.tone}>{status.word}</span>
        </button>
      </li>
    {/each}
  </ul>
  {#if next}<div class="more"><Button onclick={() => load(true)}>Показать ещё</Button></div>{/if}

  <h3 class="group-title">Встроенная память Claude</h3>
  <div class="group"><ClaudeAutoMemorySetting /></div>
</section>

{#if open}
  {#key open.id}
    <MemoryEntrySheet entry={open} {domain} onclose={() => open = null} onchanged={() => void reload()} />
  {/key}
{/if}
{#if creating}
  <MemoryEntrySheet entry={null} {domain} onclose={() => creating = false} onchanged={() => void reload()} />
{/if}

<style>
  /* Единственная колонка не шире контейнера: без minmax(0, …) длинная тема или поле поиска растягивают её по своему содержимому. */
  .memory { display: grid; grid-template-columns: minmax(0, 1fr); gap: var(--s4); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); flex-wrap: wrap; }
  h2 { margin: var(--s0); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .note, .empty { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .empty.marked { display: flex; align-items: center; gap: var(--s3); }
  .error { margin: var(--s0); color: var(--danger); }
  .tools { display: grid; gap: var(--s3); }
  .filters { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr)); gap: var(--s4); }
  .search input { display: block; white-space: nowrap; width: 100%; min-height: var(--target); padding: var(--s5) var(--s8); border: 0; border-radius: var(--radius-pill); background: var(--raised); font-size: var(--fs-input); color: inherit; }
  .list { display: grid; grid-template-columns: minmax(0, 1fr); gap: var(--s3); margin: var(--s0); padding: var(--s0); list-style: none; }
  /* white-space: normal снимает nowrap, который `.secondary-content button` задаёт кнопкам-таблеткам: он наследуется и тексту строки, и переносы не работают. */
  .row { white-space: normal; display: flex; flex-direction: column; gap: var(--s2); width: 100%; padding: var(--s6); border: 0; border-radius: var(--radius-card); background: var(--surface); color: inherit; text-align: left; }
  .row strong { overflow-wrap: anywhere; }
  .meta { color: var(--secondary); font-size: var(--fs-sm); }
  .excerpt { color: var(--muted); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .status { color: var(--muted); font-size: var(--fs-sm); font-weight: var(--weight-medium); }
  .status[data-tone='ok'] { color: var(--success); }
  .status[data-tone='warn'] { color: var(--warning); }
  @media (hover: hover) and (pointer: fine) { .row:hover { background: var(--hover); } }
  .row:focus-visible { outline: var(--focus-width) solid var(--accent); }
  .more { display: flex; justify-content: center; }
  .group-title { margin: var(--s10) var(--s6) var(--s4); color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .group { padding: var(--s6) var(--s8); border-radius: var(--radius-card); background: var(--surface); }
  .group :global(h2) { margin: var(--s0) var(--s0) var(--s4); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .group :global(section) { margin: var(--s0); }
</style>
