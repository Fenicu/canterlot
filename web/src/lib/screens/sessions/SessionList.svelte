<script module lang="ts">
  const SCROLL_KEY = 'canterlot.list-scroll';
  let savedScroll = 0;

  function rememberScroll(top: number) {
    savedScroll = top;
    sessionStorage.setItem(SCROLL_KEY, String(top));
  }

  function storedScroll(): number {
    const stored = Number(sessionStorage.getItem(SCROLL_KEY) ?? '');
    return Number.isFinite(stored) ? stored : savedScroll;
  }
</script>

<script lang="ts">
  import { listConversations, restoreConversation } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { loadConfig } from '#lib/api/config.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { browserRuntime } from '#lib/app/browser.js';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Menu from '#lib/ui/Menu.svelte';
  import MenuItem from '#lib/ui/MenuItem.svelte';
  import { theme } from '#lib/theme.js';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import { limitThresholds } from '#lib/stores/limitThresholds.svelte.js';
  import { beforeNavigate, goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import { dismissable } from '#lib/ui/dismiss.js';
  import { collapsedSet, readCollapsed, readKidsChoice, writeCollapsed, writeKidsChoice } from './collapsed';
  import EmptyState from './EmptyState.svelte';
  import GroupManager from './GroupManager.svelte';
  import GroupTabs from './GroupTabs.svelte';
  import { buildSections, tabCounts, type Tab, type TreeNode } from './grouping';
  import { readTab, resolveTab, writeTab } from './listTab';
  import LimitsStrip from './LimitsStrip.svelte';
  import { diskLimits, lowDiskNotice, type DiskNotice } from '../machines/model.js';
  import SearchBar from './SearchBar.svelte';
  import Section from './Section.svelte';
  import { rowFold } from '#lib/ui/fold.js';
  import SessionSheet from './SessionSheet.svelte';
  import TrashSection from './TrashSection.svelte';
  import TrashToast from './TrashToast.svelte';
  import { installation } from '#lib/pwa/register.js';

  let { main = false, create = false }: { main?: boolean; create?: boolean } = $props();

  let runtime = $state<Runtime | null>(null);
  let collapsed = $state<string[]>([]);
  let kidsCollapsed = $state<string[]>([]);
  let kidsExpanded = $state<string[]>([]);
  let tab = $state<Tab>(readTab());
  let query = $state('');
  let searchOpen = $state(false);
  let results = $state<ListRow[] | null>(null);
  let searchError = $state('');
  let sheetId = $state('');
  let sheetRow = $state({ top: 0, bottom: 0 });
  let listNode = $state<HTMLElement | null>(null);
  let groupsOpen = $state(false);
  let menu = $state(false);
  let canInstall = $state(false);
  let scroller = $state<HTMLElement | null>(null);
  let restore = true;
  let diskConfig = $state({ disk_warn_free_gb: 30, disk_critical_free_gb: 10 });
  let lowDiskActive = $state<string[]>([]);
  let diskNotice = $state<DiskNotice | null>(null);
  let trashToast = $state<{ id: string; key: string } | null>(null);
  let trashError = $state('');
  let trashRows = $state<ListRow[]>([]);
  let trashCursor = $state<string | null>(null);
  let trashMore = $state(false);
  let trashLoading = $state(false);
  let trashLoadError = $state('');
  let trashRequest = 0;
  const now = $derived.by(() => {
    void ticker.second;
    return clock.serverNow();
  });

  async function reloadApp() {
    await runtime?.drafts.flush().catch(() => {});
    window.location.reload();
  }

  onMount(() => {
    const install = installation();
    const changed = () => { canInstall = install.canInstall; };
    changed();
    install.addEventListener('change', changed);
    collapsed = readCollapsed();
    const showTrashToast = (id: string) => {
      sessionStorage.removeItem('canterlot.trash-toast');
      trashToast = { id, key: crypto.randomUUID() };
    };
    const onTrash = (event: Event) => {
      const id = (event as CustomEvent<string>).detail;
      if (id) showTrashToast(id);
    };
    window.addEventListener('canterlot:trash', onTrash);
    const pendingTrash = sessionStorage.getItem('canterlot.trash-toast');
    if (pendingTrash) showTrashToast(pendingTrash);
    const kids = readKidsChoice();
    kidsCollapsed = kids.collapsed;
    kidsExpanded = kids.expanded;
    let alive = true;
    void browserRuntime().then(async (value) => {
      if (!alive) return;
      runtime = value;
      // Конфиг обновляет общий реактивный порог для полосы лимитов и пороги диска.
      void loadConfig().then((config) => { diskConfig = config.machines; }).catch(() => undefined);
    }).catch(() => undefined);
    const releaseTicker = ticker.retain();
    return () => {
      install.removeEventListener('change', changed);
      window.removeEventListener('canterlot:trash', onTrash);
      alive = false;
      releaseTicker();
    };
  });

  $effect(() => {
    const text = query.trim();
    searchError = '';
    if (!text) {
      results = null;
      return;
    }
    const timer = window.setTimeout(() => {
      void listConversations({ q: text, limit: 50 }).then((response) => {
        if (query.trim() !== text) return;
        results = response.data.items as unknown as ListRow[];
      }).catch((err: unknown) => {
        // Сбой поиска не должен выглядеть как «ничего не найдено».
        if (query.trim() === text) searchError = errorText(err, 'Поиск не удался');
      });
    }, 250);
    return () => window.clearTimeout(timer);
  });

  const showTrash = $derived(tab.kind === 'trash');
  const rows = $derived(runtime ? [...runtime.list.items.values()] : []);
  const groups = $derived(runtime?.list.groups ?? []);
  const tabs = $derived(runtime ? tabCounts(runtime.list.counts, groups) : []);
  // Ответ поиска живёт дольше строки списка: удалённая сессия не должна в нём оставаться.
  // Поиск показывает и делегированные сессии плоским списком.
  const found = $derived.by((): TreeNode[] | null => {
    const store = runtime;
    if (!results || !store) return null;
    const deleted = store.list.deleted;
    return results
      .filter((item) => !deleted.has(item.id) && !store.list.items.get(item.id)?.trashed_at)
      .map((item) => ({ item, depth: 0, open: true, running: 0, waiting: 0, children: [] }));
  });
  const kids = $derived({ collapsed: collapsedSet(kidsCollapsed), expanded: collapsedSet(kidsExpanded) });
  const sections = $derived(
    found
      ? [{ id: 'search', title: 'Результаты', count: found.length, items: found, collapsed: false }]
      : buildSections(rows, groups, tab, collapsedSet(collapsed), kids)
  );
  const view = $derived(results ? `search:${query.trim()}` : JSON.stringify(tab));
  const emptyTab = $derived.by(() => {
    if (results || !ready || rows.length === 0 || sections.some((section) => section.items.length > 0)) return '';
    if (tab.kind === 'archive') return 'В архиве пока пусто';
    if (tab.kind === 'trash') return 'В корзине пусто';
    if (tab.kind === 'pinned') return 'Закреплённых сессий нет';
    return 'В этой группе пока нет сессий';
  });
  const openRow = $derived(rows.find((item) => item.id === (page.params.id ?? '')) ?? null);
  const sheetItem = $derived(
    sheetId && sections.some((section) => section.items.some((node) => node.item.id === sheetId))
      ? (rows.find((item) => item.id === sheetId) ?? null)
      : null
  );
  // Меню открыто с вкладки группы: закрепление относится к ней, а не к «Все».
  const sheetGroup = $derived.by(() => {
    if (tab.kind !== 'group') return null;
    const id = tab.id;
    return groups.find((group) => group.id === id) ?? null;
  });
  const groupCounts = $derived.by(() => {
    // Делегированные сессии групп не занимают: считают только верхний уровень.
    const counts: Record<string, number> = {};
    for (const item of rows) {
      if (item.parent_id) continue;
      for (const id of item.groups) counts[id] = (counts[id] ?? 0) + 1;
    }
    return counts;
  });

  // Строка о нехватке места живёт, пока машина не вернётся выше порога с запасом.
  $effect(() => {
    const store = runtime;
    if (!store) return;
    const machines = [...store.machines.items.values()];
    const limits = diskLimits(diskConfig);
    untrack(() => {
      const next = lowDiskNotice(machines, limits, lowDiskActive);
      if (next.active.length !== lowDiskActive.length || next.active.some((id, index) => id !== lowDiskActive[index])) {
        lowDiskActive = next.active;
      }
      if (next.notice?.text !== diskNotice?.text || next.notice?.tone !== diskNotice?.tone) {
        diskNotice = next.notice;
      }
    });
  });

  const ready = $derived(runtime ? runtime.list.status === 'live' || runtime.list.status === 'degraded' || rows.length > 0 : false);
  // Строки могут прийти из памяти устройства раньше снимка, а лимиты и счёт вкладок — только со снимком.
  const synced = $derived(runtime ? runtime.list.status === 'live' || runtime.list.status === 'degraded' : false);
  // Счёт, однажды пришедший со снимком, держится и при переподключении: без чисел вкладки перестраиваются и список прыгает.
  let counted = $state(false);
  $effect(() => {
    if (synced) counted = true;
  });

  // Выбор вкладки живёт на устройстве; удалённую группу после снимка заменяет «Все».
  $effect(() => {
    if (!synced) return;
    const next = resolveTab(tab, groups);
    if (next !== tab) tab = next;
  });
  $effect(() => {
    writeTab(tab);
  });

  // Меню принадлежит строке: смена вкладки, поиска или экрана закрывает его, как и уход строки из списка.
  // Закрытие листа подтверждения меняет запись истории, а не экран.
  const path = $derived(page.url.pathname);
  $effect(() => {
    void tab;
    void query;
    void path;
    untrack(() => {
      sheetId = '';
    });
  });
  $effect(() => {
    if (sheetId && !sheetItem) sheetId = '';
  });

  $effect(() => {
    const node = scroller;
    if (!node || rows.length === 0 || !restore) return;
    let alive = true;
    let frames = 0;
    const apply = () => {
      if (!alive) return;
      const top = storedScroll();
      node.scrollTop = top;
      frames += 1;
      if (top === 0 || node.scrollTop > 0 || frames > 12) {
        restore = false;
        return;
      }
      requestAnimationFrame(apply);
    };
    apply();
    return () => {
      alive = false;
    };
  });

  function toggle(id: string) {
    collapsed = collapsed.includes(id) ? collapsed.filter((item) => item !== id) : [...collapsed, id];
    writeCollapsed(collapsed);
  }

  function toggleKids(id: string) {
    const node = sections.flatMap((section) => section.items).find((item) => item.item.id === id);
    const isOpen = node ? node.open : kidsExpanded.includes(id);
    if (isOpen) {
      kidsCollapsed = [...kidsCollapsed.filter((item) => item !== id), id];
      kidsExpanded = kidsExpanded.filter((item) => item !== id);
    } else {
      kidsExpanded = [...kidsExpanded.filter((item) => item !== id), id];
      kidsCollapsed = kidsCollapsed.filter((item) => item !== id);
    }
    writeKidsChoice({ collapsed: kidsCollapsed, expanded: kidsExpanded });
  }

  function openSheet(id: string) {
    const row = listNode?.querySelector(`[data-session="${CSS.escape(id)}"]`);
    if (row && listNode) {
      const box = row.getBoundingClientRect();
      const origin = listNode.getBoundingClientRect().top;
      sheetRow = { top: box.top - origin, bottom: box.bottom - origin };
    }
    sheetId = sheetId === id ? '' : id;
  }

  function resetSearch() {
    query = '';
    results = null;
  }

  async function loadTrash(reset: boolean) {
    const request = ++trashRequest;
    trashLoading = true;
    trashLoadError = '';
    try {
      const response = await listConversations({
        filter: 'trashed',
        limit: 50,
        cursor: reset ? undefined : (trashCursor ?? undefined)
      });
      if (request !== trashRequest) return;
      const page = response.data;
      const pageRows = page.items as unknown as ListRow[];
      trashRows = reset ? pageRows : [...trashRows, ...pageRows];
      trashCursor = page.next_cursor ?? null;
      trashMore = Boolean(page.next_cursor);
    } catch (err) {
      if (request === trashRequest) trashLoadError = errorText(err, 'Не удалось открыть корзину');
    } finally {
      if (request === trashRequest) trashLoading = false;
    }
  }

  $effect(() => {
    const store = runtime;
    if (!store || tab.kind !== 'trash' || results) return;
    // Живые события меняют счётчик корзины — перечитываем страницу с сервера.
    const stamp = store.list.counts.trashed;
    void stamp;
    untrack(() => { void loadTrash(true); });
  });

  function ontrashed(id: string) {
    sheetId = '';
    trashToast = { id, key: crypto.randomUUID() };
    trashError = '';
    if (page.params.id === id) void goto(resolve('sessions'));
  }

  async function undoTrash() {
    const pending = trashToast;
    if (!pending) return;
    trashToast = null;
    trashError = '';
    try {
      await restoreConversation(pending.id, pending.key);
    } catch (err) {
      trashError = errorText(err, 'Не удалось вернуть сессию');
    }
  }

  function remember(event: Event) {
    if (restore) return;
    const target = event.currentTarget;
    if (target instanceof HTMLElement) rememberScroll(target.scrollTop);
  }

  beforeNavigate(({ shallow, type }) => {
    if (shallow && type === 'goto') return;
    if (scroller) rememberScroll(scroller.scrollTop);
  });
</script>

<svelte:element this={main ? 'main' : 'div'} bind:this={listNode} id={main ? 'main' : undefined} class="list" data-list-ready={ready ? '' : undefined} >
  <div class="bar" data-top-chrome>
    <p class="brand">Canterlot</p>

    <IconButton
      icon="search"
      label="Поиск"
      onclick={() => searchOpen = !searchOpen}
    />

    <IconButton
      icon={theme.effective === 'dark' ? 'sun' : 'moon'}
      label="Сменить тему"
      onclick={() => theme.toggle()}
    />

    <IconButton icon="more" label="Ещё" expanded={menu} onclick={() => menu = !menu} />

    {#if create}
      <a
        class="create"
        href={resolve('new')}
        aria-label="Новая сессия"
      ><Icon name="plus" /></a>
    {/if}

    {#if menu}
      <div class="menu-slot" use:dismissable={{ onclose: () => { menu = false; }, keep: (target) => target.closest('[aria-label="Ещё"]') !== null }}>
        <Menu label="Ещё">
          <MenuItem icon="sliders" href={resolve('/settings')} onclick={() => { menu = false; }}>Настройки</MenuItem>
          <MenuItem icon="folder" onclick={() => { groupsOpen = true; menu = false; }}>Группы</MenuItem>
          <MenuItem icon="archive" onclick={() => { tab = { kind: 'archive' }; menu = false; }}>Архив</MenuItem>
          {#if runtime && runtime.list.plan.tasks.size > 0}<MenuItem icon="list" href={resolve('/plan')} onclick={() => { menu = false; }}>Планы</MenuItem>{/if}
          <MenuItem icon="retry" onclick={() => { menu = false; void reloadApp(); }}>Обновить страницу</MenuItem>
          {#if canInstall}<MenuItem icon="download" onclick={() => { menu = false; void installation().prompt(); }}>Установить</MenuItem>{/if}
        </Menu>
      </div>
    {/if}
  </div>
  {#if searchOpen}
    <SearchBar bind:value={query} onreset={resetSearch} />
  {/if}
  {#if runtime}
    <LimitsStrip
      providers={runtime.list.limits}
      now={now}
      thresholds={limitThresholds}
      prefer={openRow?.harness.provider ?? ''}
    />
    {#if !synced && runtime.list.limits.length === 0}
      <!-- Место полосы лимитов держится до снимка, чтобы строки не съезжали вниз, когда она придёт. -->
      <div class="strip-slot" aria-hidden="true"></div>
    {/if}
    {#if diskNotice}<p class="disk-notice" data-tone={diskNotice.tone} role="status">{diskNotice.text}</p>{/if}

    <GroupTabs
      tabs={tabs}
      current={tab}
      counted={counted}
      onselect={(next) => tab = next}
    />
  {:else}
    <div class="strip-slot" aria-hidden="true"></div>
  {/if}
  <div class="scroll" data-list-scroll bind:this={scroller} onscroll={remember}>
    {#if runtime && runtime.machines.items.size === 0 && ready}
      <EmptyState kind="machines" />
    {/if}
    {#if searchError}<p class="search-error" role="alert">{searchError}</p>{/if}
    {#if results && results.length === 0}
      <EmptyState kind="search" />
    {:else if ready && rows.length === 0 && !results}
      <EmptyState kind="sessions" />
    {:else if !ready}
      <div class="placeholder" aria-hidden="true">
        {#each [56, 74, 46, 63, 51] as width (width)}
          <div class="ghost"><span class="skeleton" style:width={`var(--bar-${width})`}></span><span class="skeleton" style:width="var(--bar-81)"></span></div>
        {/each}
      </div>
    {:else if showTrash && !results}
      {#if trashLoadError}<p class="search-error" role="alert">{trashLoadError}</p>{/if}
      {#if trashLoading && trashRows.length === 0}
        <p class="trash-loading" role="status">Загружаю корзину</p>
      {:else if trashRows.length === 0}
        <EmptyState kind="tab" text="В корзине пусто" />
      {:else}
        <!-- Последняя строка корзины уходит вместе с разделом: он складывается, а не исчезает. -->
        <div data-section-slot="trash" out:rowFold>
        <TrashSection
          items={trashRows}
          {now}
          more={trashMore}
          loading={trashLoading}
          onmore={() => void loadTrash(false)}
        />
        </div>
      {/if}
    {:else if emptyTab}
      <EmptyState kind="tab" text={emptyTab} />
    {:else}
      <!-- Смена вкладки или запроса строит список заново и без движения: схлопываются только строки, ушедшие на глазах. -->
      {#key view}
      {#each sections as section (section.id)}
        <!-- Раздел, из которого ушла последняя строка, складывается вместе с заголовком. -->
        <div class="section-slot" data-section-slot={section.id} transition:rowFold>
        <Section
          {...section}
          now={now}
          current={openRow?.id ?? ''}
          heading={!results && (tab.kind === 'all' || (tab.kind === 'group' && sections.some((section) => section.id.startsWith('pinned:'))))}
          ontoggle={toggle}
          ontoggleKids={toggleKids}
          onopen={openSheet}
        />
        </div>
      {/each}
      {/key}
    {/if}
  </div>
  {#if sheetItem}
    {#key sheetItem.id}<SessionSheet
      item={sheetItem}
      groups={groups}
      group={sheetGroup}
      machine={runtime?.machines.items.get(sheetItem.machine.id) ?? null}
      anchor={sheetRow.bottom}
      anchorTop={sheetRow.top}
      ongroups={() => { sheetId = ''; groupsOpen = true; }}
      onclose={() => sheetId = ''}
      ontrashed={ontrashed}
    />{/key}
  {/if}
  {#if groupsOpen}
    <GroupManager groups={groups} counts={groupCounts} onclose={() => groupsOpen = false} />
  {/if}
  {#if trashToast}
    <TrashToast onundo={() => void undoTrash()} onclose={() => (trashToast = null)} />
  {/if}
  {#if trashError}
    <p class="trash-error" role="alert">{trashError}</p>
  {/if}
</svelte:element>

<style>
  .search-error { margin: var(--s2) var(--gutter); color: var(--danger); font-size: var(--fs-sm); }
  /* Тихая строка в семействе прочих уведомлений: цветом, без эмодзи и заливки. */
  .disk-notice { margin: var(--s0) var(--gutter) var(--s2); color: var(--warning); font-size: var(--fs-sm); }
  .disk-notice[data-tone='danger'] { color: var(--danger); }
  .trash-error { position: fixed; z-index: 6; left: 50%; bottom: calc(var(--target) + var(--s6)); transform: translateX(-50%); margin: 0; color: var(--danger); font-size: var(--fs-sm); }
  .trash-loading { margin: var(--s4) var(--gutter); color: var(--secondary); font-size: var(--fs-sm); }
  .list { position: relative; display: flex; flex: 1; flex-direction: column; min-width: 0; min-height: 0; height: 100%; }
  .bar { position: relative; display: flex; flex-wrap: wrap; align-items: center; gap: var(--s1); min-width: 0; margin: var(--s5) var(--gutter) var(--s3); padding-block: var(--s2); }
  .brand { flex: 1 0 auto; margin: 0; font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .create { display: inline-flex; align-items: center; justify-content: center; flex: none; min-width: var(--target); min-height: var(--target); border-radius: var(--radius-round); color: var(--accent-link); }
  .menu-slot { position: absolute; z-index: 4; top: calc(100% + var(--s2)); right: var(--s3); }
  /* Нижний край списка гаснет, а не режет следующую строку полосой верхушек букв над навигацией. */
  .scroll { overflow: auto; min-height: 0; flex: 1; mask-image: linear-gradient(to bottom, var(--text) calc(100% - var(--s8)), var(--transparent)); }
  .strip-slot { min-height: calc(var(--target) + var(--s2) * 2); margin: var(--s0) var(--gutter) var(--s3); }
  .placeholder { display: flex; flex-direction: column; gap: var(--s8); padding: var(--s4) var(--gutter); }
  .ghost { display: flex; flex-direction: column; gap: var(--s3); padding-inline: var(--s6); }
</style>
