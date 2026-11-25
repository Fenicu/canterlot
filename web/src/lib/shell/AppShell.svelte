<script lang="ts">
  import { page, updated } from '$app/state';
  import { version as appVersion } from '$app/env';
  import { resolve } from '$app/paths';
  import { clock } from '#lib/clock.js';
  import { connectionStale } from '#lib/sync/stale.svelte.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { ChatLease } from '#lib/screens/chat/open.js';
  import SessionList from '#lib/screens/sessions/SessionList.svelte';
  import { activeGroupId } from '#lib/screens/sessions/activeGroup.svelte.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import type { Snippet, Component } from 'svelte';
  import { onMount } from 'svelte';
  import ConnectionBanner from './ConnectionBanner.svelte';
  import { dockForm, layoutMode, type LayoutMode } from './layout';
  import { pane } from './pane.svelte';
  import Nav from './Nav.svelte';
  import { applyMotion } from '#lib/screens/settings/preferences.js';
  import { trackViewport } from './viewport';
  import { trackTypedText } from './unsaved';
  import { installation, register } from '#lib/pwa/register.js';
  import type { UpdateController } from '#lib/pwa/update.js';
  import { announce } from '#lib/a11y/announce.js';
  import { clearOwnerPreferences, followSignout, purgeLocal, watchSignout } from '#lib/pwa/signout.js';
  import { purgeAllStores } from '#lib/storage/idb.js';
  import { session, type SignOut } from '#lib/api/http.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { MachineRecord } from '#lib/stores/machines.svelte.js';
  import { goto } from '$app/navigation';
  import { hitHref, type SearchHit } from '#lib/search/model.js';
  import { cloudBannerText } from '#lib/requests/cloudWindowView.js';

  let { children }: { children: Snippet } = $props();
  let runtime = $state<Runtime | null>(null);
  let width = $state(0);
  let height = $state(0);
  // Мышь или тачпад: монитор стоя с ними раскладывается как стол.
  let mouse = $state(false);
  let laidOut = $state(false);
  let update = $state<UpdateController | null>(null);
  let updateState = $state('idle');
  let updateMessage = $state('');
  let updateError = $state('');
  const viewport = $derived({ width: width || 412, height, mouse });
  const mode: LayoutMode = $derived(layoutMode(viewport));
  // Стол стоя: правой панели нет, список чуть уже, чтобы колонке ленты осталось 600 px.
  const dock = $derived(dockForm(viewport));
  const path = $derived(page.url.pathname);
  const onSessions = $derived(path === '/' || path === '/sessions');
  const standalonePage = $derived(path === '/settings' || path === '/settings/sign-in' || path === '/changes' || path === '/share' || path === '/search' || path === '/bookmarks' || path === '/plan' || path.startsWith('/plan/'));
  // Кнопка «Новая сессия» на телефоне помнит открытую группу: форма возьмёт её прошлую сессию.
  const createHref = $derived(activeGroupId() ? `/new?group=${encodeURIComponent(activeGroupId())}` : '/new');
  const onChat = $derived(path.startsWith('/c/'));
  // Код входа и ссылка восстановления работают и без сеанса: страница входа их не закрывает.
  const authRoute = $derived(path === '/qr' || path === '/recover');
  let signOut = $state<SignOut | null>(session.cause);
  let wiped = $state(false);
  let wiping: Promise<void> | null = null;
  let SignInComponent = $state<Component<{ cause?: SignOut | null; wiped?: boolean }> | null>(null);
  const signingIn = $derived(signOut !== null && !authRoute);

  $effect(() => {
    if (signingIn && !SignInComponent) void import('#lib/auth/SignInPage.svelte').then((m) => { SignInComponent = m.default; });
  });

  /** Отзыв сеанса стирает данные устройства тем же путём, что «Очистить данные на устройстве». */
  function wipeDevice(): Promise<void> {
    wiping ??= (async () => {
      try {
        const current = runtime ?? (await browserRuntime().catch(() => null));
        if (current) await purgeLocal({ idb: current.db, caches, ls: localStorage, connection: current.connection, runtime: current });
        else { await purgeAllStores(); clearOwnerPreferences(localStorage); }
      } catch {
        // Хранилище недоступно: страница входа всё равно закрывает данные.
      }
      runtime = null;
      wiped = true;
    })();
    return wiping;
  }

  function onSession(cause: SignOut | null): void {
    // Вход вернулся (например, в другой вкладке): стёртое приложение начинает заново.
    if (!cause) { if (wiping) window.location.reload(); signOut = null; return; }
    signOut = cause;
    if (cause.wipe) void wipeDevice();
  }
  const chatId = $derived(onChat ? path.split('/')[2] : null);
  let currentChat = $state<ChatStore | null>(null);
  let DelegationPanelComponent = $state<Component<{ chat: ChatStore; stacked?: boolean; onclose?: () => void }> | null>(null);
  let MessagesPanelComponent = $state<Component<{ chat: ChatStore; onclose?: () => void }> | null>(null);
  let AlarmsPanelComponent = $state<Component<{ chat: ChatStore; onclose?: () => void }> | null>(null);
  let SearchPanelComponent = $state<Component<{ onpick: (hit: SearchHit) => void; onclose?: () => void }> | null>(null);
  let SettingsPanelComponent = $state<Component<{ chat: ChatStore; machine?: MachineRecord | null; desktop?: boolean; onclose?: () => void }> | null>(null);
  const paneMachine = $derived.by(() => {
    const id = (currentChat?.detail?.machine as { id?: string } | undefined)?.id;
    return id ? (runtime?.machines.items.get(id) ?? null) : null;
  });

  $effect(() => {
    pane.mode = mode;
    pane.dock = dock;
    pane.width = width;
  });

  $effect(() => {
    if (mode !== 'desktop') return;
    const shown = pane.shown;
    if (shown === 'delegation' && !DelegationPanelComponent) {
      import('#lib/screens/chat/DelegationPanel.svelte').then((m) => {
        DelegationPanelComponent = m.default;
      });
    }
    if (shown === 'settings' && !SettingsPanelComponent) {
      import('#lib/screens/chat/SessionSettings.svelte').then((m) => {
        SettingsPanelComponent = m.default;
      });
    }
    if (shown === 'messages' && !MessagesPanelComponent) {
      import('#lib/screens/chat/OwnerMessagesPanel.svelte').then((m) => {
        MessagesPanelComponent = m.default;
      });
    }
    if (shown === 'search' && !SearchPanelComponent) {
      import('#lib/search/SearchPanel.svelte').then((m) => {
        SearchPanelComponent = m.default;
      });
    }
    if (shown === 'alarms' && !AlarmsPanelComponent) {
      import('#lib/screens/chat/AlarmsPanel.svelte').then((m) => {
        AlarmsPanelComponent = m.default;
      });
    }
  });

  $effect(() => {
    const id = chatId;
    currentChat = null;
    if (!onChat || !id) return;
    let alive = true;
    let lease: ChatLease | null = null;
    void import('#lib/screens/chat/open.js')
      .then((module) => {
        if (!alive) return;
        lease = module.leaseChatSession(id);
        return lease.chat.then((store) => {
          if (alive) currentChat = store;
        });
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      lease?.release();
      lease = null;
    };
  });
  const onNew = $derived(path === '/new');
  const harness = $derived(path.startsWith('/__test'));
  const nav = $derived(path.startsWith('/machines') ? 'machines' : path.startsWith('/limits') ? 'limits' : path.startsWith('/presets') ? 'presets' : path.startsWith('/settings') ? 'settings' : 'sessions');
  // Баннер говорит про связь, а не про данные: его собственные часы не замирают.
  const wallNow = $derived.by(() => {
    void ticker.second;
    return clock.serverNow();
  });
  // Карточки облачных окон живут в скрытом служебном разговоре: баннер один на оболочку.
  let dismissedCloud = $state<Record<string, true>>({});
  const cloudCard = $derived(
    runtime?.list.cloudCards.find(
      (card) => dismissedCloud[card.id] !== true && card.conversation_id !== chatId
    ) ?? null
  );
  const cloudBanner = $derived(
    cloudCard
      ? {
          text: cloudBannerText(cloudCard.request_payload),
          label: 'Открыть',
          run: () => openCloudCard(cloudCard)
        }
      : null
  );
  function openCloudCard(card: { id: string; conversation_id: string }): void {
    dismissedCloud = { ...dismissedCloud, [card.id]: true };
    void goto(resolve('/c/[id]', { id: card.conversation_id }));
  }
  // Решение по карточке меняет её состояние вне событий списка: пока она открыта, снимок списка
  // перечитывается, чтобы баннер не висел над уже решённой карточкой.
  $effect(() => {
    if (!runtime || runtime.list.cloudCards.length === 0) return;
    const timer = window.setInterval(() => runtime?.list.refresh(), 60_000);
    return () => window.clearInterval(timer);
  });

  // Баннер связи уже виден пять секунд — картина замирает, пока связь не вернётся.
  $effect(() => {
    const status = runtime?.connection.status;
    if (!status) return;
    void status.phase;
    void status.since;
    void runtime?.list.status;
    void ticker.second;
    connectionStale.update(
      status.phase === 'offline' ||
        (status.phase === 'reconnecting' && clock.serverNow() - status.since >= 3000) ||
        runtime?.list.status === 'degraded'
    );
  });

  onMount(() => {
    const stopSession = session.subscribe(onSession);
    if (session.cause) onSession(session.cause);
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
    const typed = trackTypedText(document);
    void register({
      isSafe: () => !runtime?.drafts.pending && !(document.activeElement instanceof HTMLTextAreaElement && document.activeElement.closest('[data-composer]') && document.activeElement.value.length > 0),
      // Правка пузыря и текст в формах живут только на экране: черновик их не сохраняет.
      editing: () => document.querySelector('[data-unsaved-edit]') !== null || typed.pending(),
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
      height = document.documentElement.clientHeight;
    };
    const pointer = window.matchMedia('(hover: hover) and (pointer: fine)');
    const readPointer = () => {
      mouse = pointer.matches;
    };
    readPointer();
    pointer.addEventListener('change', readPointer);
    read();
    laidOut = true;
    window.addEventListener('resize', read);
    const releaseTicker = ticker.retain();
    const stopViewport = trackViewport();
    loadRuntime();
    return () => {
      alive = false;
      stopSession();
      stopSignout();
      update?.removeEventListener('change', changed);
      update?.destroy();
      typed.destroy();
      window.removeEventListener('online', retryRuntime);
      window.removeEventListener('pageshow', retryRuntime);
      document.removeEventListener('visibilitychange', retryRuntime);
      window.removeEventListener('canterlot:incompatible', incompatible);
      window.removeEventListener('focus', checkUpdate);
      window.removeEventListener('pageshow', checkUpdate);
      window.removeEventListener('online', checkUpdate);
      document.removeEventListener('visibilitychange', visibility);
      window.removeEventListener('resize', read);
      pointer.removeEventListener('change', readPointer);
      stopViewport();
      releaseTicker();
    };
  });

  /**
   * Совпадение из панели поиска стола: та же ссылка, что на телефоне. Повторное нажатие
   * на совпадение открытой ленты адреса не меняет, поэтому прыжок просится напрямую.
   */
  function pickHit(hit: SearchHit): void {
    const href = hitHref(hit);
    const chat = currentChat;
    if (chat && chat.id === hit.conversation_id && `${page.url.pathname}${page.url.search}` === href) {
      if (hit.block_id) chat.requestBlockJump(hit.block_id, String(hit.jump_seq));
      else if (hit.input_id) chat.requestJump(hit.input_id, String(hit.jump_seq));
      return;
    }
    // eslint-disable-next-line svelte/no-navigation-without-resolve
    void goto(href);
  }

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

<div class="shell" data-layout={laidOut ? mode : undefined} data-harness={harness ? '' : undefined} data-stale={connectionStale.stale ? '' : undefined}>
  {#snippet banner(ownSlot = false)}
    <ConnectionBanner
      phase={runtime?.connection.status.phase ?? 'connecting'}
      since={runtime?.connection.status.since ?? 0}
      now={wallNow}
      scope={runtime?.list.status ?? 'idle'}
      stale={connectionStale.stale}
      lastAt={runtime?.connection.status.lastFrameAt ?? 0}
      {ownSlot}
      update={updateState}
      updateMessage={updateMessage}
      failure={updateError}
      cloud={cloudBanner}
      onupdate={applyUpdate}
      onrefresh={() => { void runtime?.drafts.flush().then(() => window.location.reload()); }}
    />
  {/snippet}
  {#if signingIn}
    {#if SignInComponent}<SignInComponent cause={signOut} {wiped} />{/if}
  {:else if harness || authRoute}
    {@render banner()}
    <div class="page">{@render children()}</div>
  {:else if laidOut && mode === 'phone'}
    {#if onSessions || onNew}
      {@render banner(true)}
      <SessionList main={onSessions} />
    {:else}
      {@render banner()}
    {/if}

    {#if !onSessions}
      <div class="page">{@render children()}</div>
    {/if}
    {#if !onChat && !standalonePage}
      <Nav current={nav} create={onSessions || onNew} {createHref} />
    {/if}
  {:else if laidOut}
    <div class="panes" data-dock={dock ? '' : undefined}>
      <!-- Стол: группы и разделы в полосе слева, нижней навигации в панели списка нет. -->
      <aside data-pane="list" data-railed={mode === 'desktop' ? '' : undefined}>
        <SessionList create rail={mode === 'desktop'} section={nav} />
        {#if mode !== 'desktop'}<Nav current={nav} create={false} />{/if}
      </aside>
      <div data-pane="chat">
        {@render banner()}
        {@render children()}
      </div>
      {#if pane.lane && pane.shown && (onChat || pane.shown === 'search')}
        <aside data-pane="lane">
          {#if pane.shown === 'search' && SearchPanelComponent}
            <SearchPanelComponent onpick={pickHit} onclose={() => pane.close()} />
          {:else if currentChat && pane.shown === 'delegation' && DelegationPanelComponent}
            <DelegationPanelComponent chat={currentChat} stacked={pane.stacked} onclose={() => pane.closeDelegation()} />
          {:else if currentChat && pane.shown === 'settings' && SettingsPanelComponent}
            <SettingsPanelComponent chat={currentChat} machine={paneMachine} desktop onclose={() => pane.close()} />
          {:else if currentChat && pane.shown === 'messages' && MessagesPanelComponent}
            <MessagesPanelComponent chat={currentChat} onclose={() => pane.close()} />
          {:else if currentChat && pane.shown === 'alarms' && AlarmsPanelComponent}
            <AlarmsPanelComponent chat={currentChat} onclose={() => pane.close()} />
          {/if}
        </aside>
      {/if}
    </div>
  {:else}
    {@render banner()}
  {/if}
</div>

<style>
  .shell {
    position: fixed;
    inset: var(--s0) var(--s0) var(--keyboard-inset) var(--s0);
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .shell[data-harness] {
    position: relative;
    height: var(--app-height);
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

  /* Стол стоя: список уже, колонке ленты остаётся не меньше 600 px. */
  .panes[data-dock] {
    --sidebar-width: var(--sidebar-portrait);
  }

  [data-pane="list"][data-railed] {
    width: calc(var(--sidebar-width) + var(--rail-width));
    flex-basis: calc(var(--sidebar-width) + var(--rail-width));
  }

  /* Баннер связи лежит в панели чата поверх содержимого, не в потоке. */
  [data-pane="chat"] {
    position: relative;
    flex: 1;
    display: flex;
    flex-direction: column;
  }

  [data-pane="lane"] {
    width: var(--lane-width);
    flex: 0 0 var(--lane-width);
  }
</style>
