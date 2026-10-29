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
  import { listConversations } from '$lib/api/client';
  import { loadConfig } from '$lib/api/config';
  import type { Runtime } from '$lib/app/runtime';
  import { clock } from '$lib/clock';
  import { browserRuntime } from '$lib/screens/chat/open';
  import Icon from '$lib/ui/Icon.svelte';
  import IconButton from '$lib/ui/IconButton.svelte';
  import { theme } from '$lib/theme';
  import type { ListRow } from '$lib/stores/list.svelte';
  import type { LimitThresholds } from '$lib/stores/limits';
  import { beforeNavigate } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { collapsedSet, readCollapsed, writeCollapsed } from './collapsed';
  import EmptyState from './EmptyState.svelte';
  import GroupManager from './GroupManager.svelte';
  import GroupTabs from './GroupTabs.svelte';
  import { buildSections, tabCounts, type Tab } from './grouping';
  import LimitsStrip from './LimitsStrip.svelte';
  import SearchBar from './SearchBar.svelte';
  import Section from './Section.svelte';
  import SessionSheet from './SessionSheet.svelte';
  import { installation } from '$lib/pwa/register';

  let { main = false, create = false }: { main?: boolean; create?: boolean } = $props();

  let runtime = $state<Runtime | null>(null);
  let collapsed = $state<string[]>([]);
  let tab = $state<Tab>({ kind: 'all' });
  let query = $state('');
  let searchOpen = $state(false);
  let results = $state<ListRow[] | null>(null);
  let sheetId = $state('');
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

  onMount(() => {
    const install = installation();
    const changed = () => { canInstall = install.canInstall; };
    changed();
    install.addEventListener('change', changed);
    collapsed = readCollapsed();
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
  const sections = $derived(
    results
      ? [{ id: 'search', title: 'Результаты', count: results.length, items: results, collapsed: false }]
      : buildSections(rows, groups, tab, collapsedSet(collapsed))
  );
  const openRow = $derived(rows.find((item) => item.id === (page.params.id ?? '')) ?? null);
  const sheetItem = $derived(rows.find((item) => item.id === sheetId) ?? null);
  const ready = $derived(runtime ? runtime.list.status === 'live' || runtime.list.status === 'degraded' || rows.length > 0 : false);

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

  function resetSearch() {
    query = '';
    results = null;
  }

  function remember(event: Event) {
    if (restore) return;
    const target = event.currentTarget;
    if (target instanceof HTMLElement) rememberScroll(target.scrollTop);
  }

  beforeNavigate(() => {
    if (scroller) rememberScroll(scroller.scrollTop);
  });
</script>

<svelte:element this={main ? 'main' : 'div'} id={main ? 'main' : undefined} class="list" data-list-ready={ready ? '' : undefined}>
  <div class="bar">
    <p class="brand">canterlot</p>
    {#if create}<a class="create" href={resolve('/new')} aria-label="Новая сессия"><span aria-hidden="true">+</span></a>{/if}
    <IconButton icon="search" label="Поиск" onclick={() => (searchOpen = !searchOpen)} />
    <IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />
    <button type="button" aria-label="Ещё" aria-expanded={menu} onclick={() => (menu = !menu)}><Icon name="more" /></button>
    {#if menu}
      <div class="menu" role="menu">
        <a role="menuitem" href={resolve('/settings')} onclick={() => { menu = false; }}>Настройки</a>
        <button type="button" role="menuitem" onclick={() => { groupsOpen = true; menu = false; }}>Группы</button>
        <button type="button" role="menuitem" onclick={() => { tab = { kind: 'archive' }; menu = false; }}>Архив</button>
        {#if canInstall}<button type="button" role="menuitem" onclick={() => { menu = false; void installation().prompt(); }}>Установить</button>{/if}
      </div>
    {/if}
  </div>
  {#if searchOpen}
    <SearchBar bind:value={query} onreset={resetSearch} />
  {/if}
  {#if runtime}
    <LimitsStrip providers={runtime.list.limits} {now} {thresholds} prefer={openRow?.harness.provider ?? ''} crunch={openRow?.crunch.enabled ?? false} />
    <GroupTabs {tabs} current={tab} onselect={(next) => (tab = next)} />
  {/if}
  <div class="scroll" data-list-scroll bind:this={scroller} onscroll={remember}>
    {#if runtime && runtime.machines.items.size === 0 && ready}
      <EmptyState kind="machines" />
    {/if}
    {#if results && results.length === 0}
      <EmptyState kind="search" onreset={resetSearch} />
    {:else if ready && rows.length === 0 && !results}
      <EmptyState kind="sessions" />
    {:else}
      {#each sections as section (section.id)}
        <Section {...section} {now} ontoggle={toggle} onopen={(id) => (sheetId = id)} />
      {/each}
    {/if}
  </div>
  {#if sheetItem}
    <SessionSheet item={sheetItem} {groups} onclose={() => (sheetId = '')} />
  {/if}
  {#if groupsOpen}
    <GroupManager {groups} onclose={() => (groupsOpen = false)} />
  {/if}
</svelte:element>

<style>
  .list { position: relative; display: flex; flex: 1; flex-direction: column; min-width: 0; min-height: 0; height: 100%; }
  .bar { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s2); min-width: 0; margin: var(--s5) var(--s6) var(--s3); padding: var(--s4); border-radius: var(--radius-pill); background: var(--surface); }
  .brand { flex: 1; min-width: 0; margin: 0; padding-left: var(--s4); font-size: var(--fs-title); }
  .create { display: inline-flex; align-items: center; justify-content: center; min-width: var(--target); min-height: var(--target); border-radius: var(--radius-round); background: var(--accent); color: var(--accent-ink); }
  .menu { position: absolute; z-index: 2; right: var(--s6); top: var(--s14); display: flex; flex-direction: column; padding: var(--s4); border-radius: var(--radius-card); background: var(--surface); }
  .menu button, .bar > button { min-width: var(--target); min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--text); }
  .scroll { overflow: auto; min-height: 0; flex: 1; }
</style>
