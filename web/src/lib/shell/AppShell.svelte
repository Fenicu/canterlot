<script lang="ts">
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { clock } from '$lib/clock';
  import { browserRuntime, openChatSession } from '$lib/screens/chat/open';
  import SessionList from '$lib/screens/sessions/SessionList.svelte';
  import type { Runtime } from '$lib/app/runtime';
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import ConnectionBanner from './ConnectionBanner.svelte';
  import { layoutMode, type LayoutMode } from './layout';
  import Nav from './Nav.svelte';
  import SessionExpired from './SessionExpired.svelte';
  import { installation, register } from '$lib/pwa/register';
  import type { UpdateController } from '$lib/pwa/update';
  import { staleness } from '$lib/pwa/offline';
  import { announce } from '$lib/a11y/announce';
  import DelegationPanel from '$lib/screens/chat/DelegationPanel.svelte';
  import type { ChatStore } from '$lib/stores/chat.svelte';

  let { children }: { children: Snippet } = $props();
  let runtime = $state<Runtime | null>(null);
  let width = $state(0);
  let laidOut = $state(false);
  let tick = $state(0);
  let lane = $state(false);
  let update = $state<UpdateController | null>(null);
  let updateState = $state('idle');
  let offlineLabel = $state('');
  let updateError = $state('');
  const mode: LayoutMode = $derived(layoutMode(width || 412));
  const path = $derived(page.url.pathname);
  const onSessions = $derived(path === '/' || path === '/sessions');
  const onChat = $derived(path.startsWith('/c/'));
  const chatId = $derived(onChat ? path.split('/')[2] : null);
  let currentChat = $state<ChatStore | null>(null);

  $effect(() => {
    const id = chatId;
    if (onChat && id) {
      let alive = true;
      openChatSession(id)
        .then((store) => {
          if (alive) currentChat = store;
        })
        .catch(() => {});
      return () => {
        alive = false;
      };
    } else {
      currentChat = null;
    }
  });
  const onNew = $derived(path === '/new');
  const harness = $derived(path.startsWith('/__test'));
  const nav = $derived(path.startsWith('/machines') ? 'machines' : path.startsWith('/limits') ? 'limits' : 'sessions');
  const now = $derived.by(() => {
    void tick;
    return clock.serverNow();
  });

  onMount(() => {
    installation();
    let alive = true;
    const offlineChanged = () => { offlineLabel = staleness.label(Date.now()); };
    const online = () => staleness.setOnline(navigator.onLine);
    staleness.addEventListener('change', offlineChanged);
    window.addEventListener('online', online);
    window.addEventListener('offline', online);
    online();
    const changed = () => { updateState = update?.state ?? 'idle'; };
    const incompatible = () => update?.incompatible();
    window.addEventListener('canterlot:incompatible', incompatible);
    void register({
      isSafe: () => !runtime?.drafts.pending && !(document.activeElement instanceof HTMLTextAreaElement && document.activeElement.closest('[data-composer]') && document.activeElement.value.length > 0),
      flushDrafts: async () => { await runtime?.drafts.flush(); }
    }).then(value => {
      if (!alive) { value?.destroy(); return; }
      update = value;
      update?.addEventListener('change', changed);
      changed();
    }).catch(() => { updateError = 'Не удалось подготовить обновление. Повторите при восстановлении связи'; });
    const visibility = () => {
      if (document.hidden && update?.state === 'ready') void applyUpdate();
      else if (!document.hidden) void update?.check().catch(() => undefined);
    };
    document.addEventListener('visibilitychange', visibility);
    const read = () => {
      width = document.documentElement.clientWidth;
    };
    read();
    laidOut = true;
    window.addEventListener('resize', read);
    const timer = window.setInterval(() => tick++, 1000);
    void browserRuntime().then((value) => {
      runtime = value;
      offlineChanged();
    }).catch(() => undefined);
    return () => {
      alive = false;
      update?.removeEventListener('change', changed);
      update?.destroy();
      staleness.removeEventListener('change', offlineChanged);
      window.removeEventListener('online', online);
      window.removeEventListener('offline', online);
      window.removeEventListener('canterlot:incompatible', incompatible);
      document.removeEventListener('visibilitychange', visibility);
      window.removeEventListener('resize', read);
      window.clearInterval(timer);
    };
  });

  async function applyUpdate() {
    try {
      await runtime?.drafts.flush();
      await update?.apply();
    } catch { updateError = 'Не удалось сохранить черновик. Обновление отложено'; }
  }
  $effect(() => {
    if (runtime?.connection.status.phase === 'incompatible') update?.incompatible();
  });
  $effect(() => {
    if (offlineLabel) announce(offlineLabel);
    else if (updateState !== 'idle') announce('Доступна новая версия');
  });
</script>

<div class="shell" data-layout={laidOut ? mode : undefined}>
  {#if offlineLabel}<p class="platform-banner" data-offline>{offlineLabel}</p>{/if}
  {#if updateState !== 'idle'}
    <p class="platform-banner" data-update>
      Доступна новая версия
      {#if updateState === 'applying'}Сохраняю и обновляю…{:else}<button type="button" onclick={applyUpdate}>Обновить</button>{/if}
    </p>
  {/if}
  {#if updateError}<p class="platform-banner">{updateError}</p>{/if}
  {#if runtime}
    <ConnectionBanner
      phase={runtime.connection.status.phase}
      since={runtime.connection.status.since}
      {now}
      scope={runtime.list.status}
      onrefresh={() => { void runtime?.drafts.flush().then(() => window.location.reload()); }}
    />
    <SessionExpired phase={runtime.connection.status.phase} {now} />
  {/if}
  {#if harness}
    <div class="page">{@render children()}</div>
  {:else if laidOut && mode === 'phone'}
    {#if onSessions || onNew}
      <SessionList main={onSessions} />
    {/if}
    {#if onChat}<a class="back" href={resolve('/sessions')}>Назад</a>{/if}
    {#if !onSessions}
      <div class="page">{@render children()}</div>
    {/if}
    {#if !onChat}
      <Nav current={nav} />
    {/if}
  {:else if laidOut}
    <div class="panes">
      <aside data-pane="list"><SessionList create /></aside>
      <div data-pane="chat">
        {#if mode === 'desktop' && onChat}
          <button type="button" onclick={() => (lane = !lane)}>{lane ? 'Скрыть поручения' : 'Поручения'}</button>
        {/if}
        {@render children()}
      </div>
      {#if mode === 'desktop' && lane}
        <aside data-pane="lane">
          {#if currentChat}
            <DelegationPanel chat={currentChat} onclose={() => (lane = false)} />
          {:else}
            <p>Поручения</p>
          {/if}
        </aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .shell { height: 100dvh; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .page { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .page :global(main), [data-pane='chat'] :global(main) { flex: 1; min-height: 0; height: 100%; display: flex; flex-direction: column; }
  .back { min-height: var(--target); padding: var(--s4) var(--s6); color: var(--accent-link); }
  .panes { flex: 1; min-height: 0; display: flex; gap: var(--s6); padding: var(--s6); }
  [data-pane] { min-width: 0; min-height: 0; overflow: hidden; display: flex; flex-direction: column; border-radius: var(--radius-sheet); background: var(--surface); }
  [data-pane='list'] { width: var(--sidebar-width); flex: 0 0 var(--sidebar-width); }
  [data-pane='chat'] { flex: 1; display: flex; flex-direction: column; }
  [data-pane='lane'] { width: var(--lane-width); flex: 0 0 var(--lane-width); padding: var(--s6); }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--accent-link); }
  .platform-banner { margin: var(--s4) var(--s6) 0; padding: var(--s4) var(--s6); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); }
  .platform-banner button { margin-left: var(--s4); }
</style>
