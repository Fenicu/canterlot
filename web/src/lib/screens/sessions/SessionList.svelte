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
  import { listConversations } from '#lib/api/client.js';
  import { loadConfig } from '#lib/api/config.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { clock } from '#lib/clock.js';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Menu from '#lib/ui/Menu.svelte';
  import MenuItem from '#lib/ui/MenuItem.svelte';
  import { theme } from '#lib/theme.js';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import type { LimitThresholds } from '#lib/stores/limits.js';
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
  import LimitsStrip from './LimitsStrip.svelte';
  import SearchBar from './SearchBar.svelte';
  import Section from './Section.svelte';
  import SessionSheet from './SessionSheet.svelte';
  import { installation } from '#lib/pwa/register.js';

  let { main = false, create = false }: { main?: boolean; create?: boolean } = $props();

  let runtime = $state<Runtime | null>(null);
  let collapsed = $state<string[]>([]);
  let kidsCollapsed = $state<string[]>([]);
  let kidsExpanded = $state<string[]>([]);
  let tab = $state<Tab>({ kind: 'all' });
  let query = $state('');
  let searchOpen = $state(false);
  let results = $state<ListRow[] | null>(null);
  let sheetId = $state('');
  let sheetRow = $state({ top: 0, bottom: 0 });
  let listNode = $state<HTMLElement | null>(null);
  let groupsOpen = $state(false);
  let menu = $state(false);
  let canInstall = $state(false);
  let tick = $state(0);
  let scroller = $state<HTMLElement | null>(null);
  let thresholds = $state<LimitThresholds>({ stale_after_secs: 35 * 60, warn_pcts: [80], weekly_reserve_pct: 95 });
  let restore = true;
  const now = $derived.by(() => {
    void tick;
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
    const kids = readKidsChoice();
    kidsCollapsed = kids.collapsed;
    kidsExpanded = kids.expanded;
    let alive = true;
    void browserRuntime().then(async (value) => {
      if (!alive) return;
      runtime = value;
      const config = await loadConfig().catch(() => null);
      if (config?.limit_thresholds) thresholds = config.limit_thresholds;
    }).catch(() => undefined);
    let timer = 0;
    const arm = () => {
      timer = window.setInterval(() => tick++, 1000);
    };
    const visible = () => {
      if (document.hidden) {
        window.clearInterval(timer);
        timer = 0;
      } else if (!timer) arm();
    };
    arm();
    document.addEventListener('visibilitychange', visible);
    return () => {
      install.removeEventListener('change', changed);
      alive = false;
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', visible);
    };
  });

  $effect(() => {
    const text = query.trim();
    if (!text) {
      results = null;
      return;
    }
    const timer = window.setTimeout(() => {
      void listConversations({ q: text, limit: 50 }).then((response) => {
        if (query.trim() !== text) return;
        results = response.data.items as unknown as ListRow[];
      }).catch(() => undefined);
    }, 250);
    return () => window.clearTimeout(timer);
  });

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
      .filter((item) => !deleted.has(item.id))
      .map((item) => ({ item, depth: 0, open: true, running: 0, waiting: 0, children: [] }));
  });
  const kids = $derived({ collapsed: collapsedSet(kidsCollapsed), expanded: collapsedSet(kidsExpanded) });
  const sections = $derived(
    found
      ? [{ id: 'search', title: 'Результаты', count: found.length, items: found, collapsed: false }]
      : buildSections(rows, groups, tab, collapsedSet(collapsed), kids)
  );
  const emptyTab = $derived.by(() => {
    if (results || !ready || rows.length === 0 || sections.some((section) => section.items.length > 0)) return '';
    if (tab.kind === 'archive') return 'В архиве пока пусто';
    if (tab.kind === 'pinned') return 'Закреплённых сессий нет';
    return 'В этой группе пока нет сессий';
  });
  const openRow = $derived(rows.find((item) => item.id === (page.params.id ?? '')) ?? null);
  const sheetItem = $derived(
    sheetId && sections.some((section) => section.items.some((node) => node.item.id === sheetId))
      ? (rows.find((item) => item.id === sheetId) ?? null)
      : null
  );
  const groupCounts = $derived.by(() => {
    // Делегированные сессии групп не занимают: считают только верхний уровень.
    const counts: Record<string, number> = {};
    for (const item of rows) {
      if (item.parent_id) continue;
      for (const id of item.groups) counts[id] = (counts[id] ?? 0) + 1;
    }
    return counts;
  });
  const ready = $derived(runtime ? runtime.list.status === 'live' || runtime.list.status === 'degraded' || rows.length > 0 : false);

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

<svelte:element this={main ? 'main' : 'div'} bind:this={listNode} id={main ? 'main' : undefined} class="list" data-list-ready={ready ? '' : undefined}>
  <div class="bar">
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
      thresholds={thresholds}
      prefer={openRow?.harness.provider ?? ''}
    />

    <GroupTabs
      tabs={tabs}
      current={tab}
      onselect={(next) => tab = next}
    />
  {/if}
  <div class="scroll" data-list-scroll bind:this={scroller} onscroll={remember}>
    {#if runtime && runtime.machines.items.size === 0 && ready}
      <EmptyState kind="machines" />
    {/if}
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
    {:else if emptyTab}
      <EmptyState kind="tab" text={emptyTab} />
    {:else}
      {#each sections as section (section.id)}
        <Section
          {...section}
          now={now}
          current={openRow?.id ?? ''}
          heading={tab.kind === 'all' && !results}
          ontoggle={toggle}
          ontoggleKids={toggleKids}
          onopen={openSheet}
        />
      {/each}
    {/if}
  </div>
  {#if sheetItem}
    {#key sheetItem.id}<SessionSheet
      item={sheetItem}
      groups={groups}
      machine={runtime?.machines.items.get(sheetItem.machine.id) ?? null}
      anchor={sheetRow.bottom}
      anchorTop={sheetRow.top}
      ongroups={() => { sheetId = ''; groupsOpen = true; }}
      onclose={() => sheetId = ''}
      ondeleted={(id) => { if (page.params.id === id) void goto(resolve('sessions')); }}
    />{/key}
  {/if}
  {#if groupsOpen}
    <GroupManager groups={groups} counts={groupCounts} onclose={() => groupsOpen = false} />
  {/if}
</svelte:element>

<style>
  .list { position: relative; display: flex; flex: 1; flex-direction: column; min-width: 0; min-height: 0; height: 100%; }
  .bar { position: relative; display: flex; flex-wrap: wrap; align-items: center; gap: var(--s1); min-width: 0; margin: var(--s5) var(--gutter) var(--s3); padding-block: var(--s2); }
  .brand { flex: 1 0 auto; margin: 0; font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .create { display: inline-flex; align-items: center; justify-content: center; flex: none; min-width: var(--target); min-height: var(--target); border-radius: var(--radius-round); color: var(--accent-link); }
  .menu-slot { position: absolute; z-index: 4; top: calc(100% + var(--s2)); right: var(--s3); }
  .scroll { overflow: auto; min-height: 0; flex: 1; }
  .placeholder { display: flex; flex-direction: column; gap: var(--s8); padding: var(--s4) var(--gutter); }
  .ghost { display: flex; flex-direction: column; gap: var(--s3); padding-inline: var(--s6); }
</style>
