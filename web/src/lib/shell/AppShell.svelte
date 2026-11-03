<script lang="ts">
  import { page, updated } from '$app/state';
  import { version as appVersion } from '$app/env';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { browserRuntime, leaseChatSession } from '#lib/screens/chat/open.js';
  import SessionList from '#lib/screens/sessions/SessionList.svelte';
  import type { Runtime } from '#lib/app/runtime.js';
  import type { Snippet, Component } from 'svelte';
  import { onMount } from 'svelte';
  import ConnectionBanner from './ConnectionBanner.svelte';
  import { layoutMode, type LayoutMode } from './layout';
  import { pane } from './pane.svelte';
  import Nav from './Nav.svelte';
  import SessionExpired from './SessionExpired.svelte';
  import { applyMotion } from '#lib/screens/settings/preferences.js';
  import { installation, register } from '#lib/pwa/register.js';
  import type { UpdateController } from '#lib/pwa/update.js';
  import { announce } from '#lib/a11y/announce.js';
  import { followSignout, watchSignout } from '#lib/pwa/signout.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { MachineRecord } from '#lib/stores/machines.svelte.js';

  let { children }: { children: Snippet } = $props();
  let runtime = $state<Runtime | null>(null);
  let width = $state(0);
  let laidOut = $state(false);
  let update = $state<UpdateController | null>(null);
  let updateState = $state('idle');
  let updateMessage = $state('');
  let updateError = $state('');
  const mode: LayoutMode = $derived(layoutMode(width || 412));
  const path = $derived(page.url.pathname);
  const onSessions = $derived(path === '/' || path === '/sessions');
  const standalonePage = $derived(path === '/settings' || path === '/changes' || path === '/share');
  const onChat = $derived(path.startsWith('/c/'));
  const chatId = $derived(onChat ? path.split('/')[2] : null);
  let currentChat = $state<ChatStore | null>(null);
  let DelegationPanelComponent = $state<Component<{ chat: ChatStore; onclose?: () => void }> | null>(null);
  let SettingsPanelComponent = $state<Component<{ chat: ChatStore; machine?: MachineRecord | null; desktop?: boolean; onclose?: () => void }> | null>(null);
  const paneMachine = $derived.by(() => {
    const id = (currentChat?.detail?.machine as { id?: string } | undefined)?.id;
    return id ? (runtime?.machines.items.get(id) ?? null) : null;
  });

  $effect(() => {
    pane.mode = mode;
  });

  $effect(() => {
    if (mode !== 'desktop') return;
    if (pane.view === 'delegation' && !DelegationPanelComponent) {
      import('#lib/screens/chat/DelegationPanel.svelte').then((m) => {
        DelegationPanelComponent = m.default;
      });
    }
    if (pane.view === 'settings' && !SettingsPanelComponent) {
      import('#lib/screens/chat/SessionSettings.svelte').then((m) => {
        SettingsPanelComponent = m.default;
      });
    }
  });

  $effect(() => {
    const id = chatId;
    currentChat = null;
    if (onChat && id) {
      let alive = true;
      const lease = leaseChatSession(id);
      lease.chat
        .then((store) => {
          if (alive) currentChat = store;
        })
        .catch(() => {});
      return () => {
        alive = false;
        lease.release();
      };
    } else {
      currentChat = null;
    }
  });
  const onNew = $derived(path === '/new');
  const harness = $derived(path.startsWith('/__test'));
  const nav = $derived(path.startsWith('/machines') ? 'machines' : path.startsWith('/limits') ? 'limits' : path.startsWith('/presets') ? 'presets' : 'sessions');
  const now = $derived.by(() => {
    void ticker.second;
    return clock.serverNow();
  });

  onMount(() => {
    installation();
    applyMotion();
    let alive = true;
    const stopSignout = watchSignout(() => {
      void followSignout({ ls: localStorage, reload: () => window.location.reload() });
    });
    const loadRuntime = () => {
      void browserRuntime().then((value) => {
        runtime = value;
      }).catch(() => undefined);
    };
    const retryRuntime = () => {
      if (!runtime && !document.hidden) loadRuntime();
    };
    window.addEventListener('online', retryRuntime);
    window.addEventListener('pageshow', retryRuntime);
    document.addEventListener('visibilitychange', retryRuntime);
    const changed = () => { updateState = update?.state ?? 'idle'; updateMessage = update?.message ?? ''; };
    const incompatible = () => update?.incompatible();
    const checkUpdate = () => { void update?.check().catch(() => undefined); };
    window.addEventListener('canterlot:incompatible', incompatible);
    window.addEventListener('focus', checkUpdate);
    window.addEventListener('pageshow', checkUpdate);
    window.addEventListener('online', checkUpdate);
    void register({
      isSafe: () => !runtime?.drafts.pending && !(document.activeElement instanceof HTMLTextAreaElement && document.activeElement.closest('[data-composer]') && document.activeElement.value.length > 0),
      // Правка пузыря живёт только на экране: черновик её не сохраняет.
      editing: () => document.querySelector('[data-unsaved-edit]') !== null,
      flushDrafts: async () => {
        await runtime?.drafts.flush();
      },
      version: appVersion
    }).then((value) => {
      if (!alive) {
        value?.destroy();

        return;
      }

      update = value;
      update?.addEventListener('change', changed);
      changed();
      void update?.check().catch(() => undefined);
    }).catch(() => { updateError = 'Не удалось подготовить обновление. Повторите при восстановлении связи'; });
    const visibility = () => {
      if (document.hidden && update?.state === 'available') void applyUpdate(false);
      else if (!document.hidden) void update?.check().catch(() => undefined);
    };
    document.addEventListener('visibilitychange', visibility);
    const read = () => {
      width = document.documentElement.clientWidth;
    };
    read();
    laidOut = true;
    window.addEventListener('resize', read);
    const releaseTicker = ticker.retain();
    loadRuntime();
    return () => {
      alive = false;
      stopSignout();
      update?.removeEventListener('change', changed);
      update?.destroy();
      window.removeEventListener('online', retryRuntime);
      window.removeEventListener('pageshow', retryRuntime);
      document.removeEventListener('visibilitychange', retryRuntime);
      window.removeEventListener('canterlot:incompatible', incompatible);
      window.removeEventListener('focus', checkUpdate);
      window.removeEventListener('pageshow', checkUpdate);
      window.removeEventListener('online', checkUpdate);
      document.removeEventListener('visibilitychange', visibility);
      window.removeEventListener('resize', read);
      releaseTicker();
    };
  });

  async function applyUpdate(force = true) {
    try {
      await runtime?.drafts.flush();
      await update?.apply(force);
    } catch { updateError = 'Не удалось сохранить черновик. Обновление отложено'; }
  }
  $effect(() => {
    if (updated.current) update?.notify();
  });
  $effect(() => {
    if (runtime?.connection.status.phase === 'incompatible') update?.incompatible();
  });
  $effect(() => {
    if (updateState === 'failed' && updateMessage) announce(updateMessage);
    else if (updateState === 'available') announce('Доступна новая версия');
  });
</script>

<div class="shell" data-layout={laidOut ? mode : undefined} data-harness={harness ? '' : undefined}>
  {#snippet banner()}
    <ConnectionBanner
      phase={runtime?.connection.status.phase ?? 'connecting'}
      since={runtime?.connection.status.since ?? 0}
      now={now}
      scope={runtime?.list.status ?? 'idle'}
      update={updateState}
      updateMessage={updateMessage}
      failure={updateError}
      onupdate={applyUpdate}
      onrefresh={() => { void runtime?.drafts.flush().then(() => window.location.reload()); }}
      onrelogin={() => window.location.assign('/')}
    />
  {/snippet}
  {#if harness || !laidOut || mode === 'phone'}{@render banner()}{/if}
  {#if runtime}<SessionExpired phase={runtime.connection.status.phase} now={now} />{/if}
  {#if harness}
    <div class="page">{@render children()}</div>
  {:else if laidOut && mode === 'phone'}
    {#if onSessions || onNew}
      <SessionList main={onSessions} />
    {/if}

    {#if !onSessions}
      <div class="page">{@render children()}</div>
    {/if}
    {#if !onChat && !standalonePage}
      <Nav current={nav} create={onSessions || onNew} />
    {/if}
  {:else if laidOut}
    <div class="panes">
      <aside data-pane="list"><SessionList create /><Nav current={nav} create={false} /></aside>
      <div data-pane="chat">
        {@render banner()}
        {@render children()}
      </div>
      {#if mode === 'desktop' && onChat && pane.view}
        <aside data-pane="lane">
          {#if currentChat && pane.view === 'delegation' && DelegationPanelComponent}
            <DelegationPanelComponent chat={currentChat} onclose={() => pane.close()} />
          {:else if currentChat && pane.view === 'settings' && SettingsPanelComponent}
            <SettingsPanelComponent chat={currentChat} machine={paneMachine} desktop onclose={() => pane.close()} />
          {/if}
        </aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .shell {
    position: fixed;
    inset: var(--s0);
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .shell[data-harness] {
    position: static;
    height: 100dvh;
  }

  .page {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .page :global(main),
  [data-pane="chat"] :global(main) {
    flex: 1;
    min-height: 0;
    height: 100%;
    display: flex;
    flex-direction: column;
  }

  .panes {
    flex: 1;
    min-height: 0;
    display: flex;
    gap: var(--s6);
    padding: var(--s6);
  }

  [data-pane] {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    border-radius: var(--radius-sheet);
    background: var(--chat-bg);
  }

  [data-pane="list"] {
    width: var(--sidebar-width);
    flex: 0 0 var(--sidebar-width);
  }

  [data-pane="chat"] {
    flex: 1;
    display: flex;
    flex-direction: column;
  }

  [data-pane="lane"] {
    width: var(--lane-width);
    flex: 0 0 var(--lane-width);
  }
</style>
