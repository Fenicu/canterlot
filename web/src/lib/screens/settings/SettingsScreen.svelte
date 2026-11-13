<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { loadConfig } from '#lib/api/config.js';
  import { me } from '#lib/api/client.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { compareVersions } from '#lib/changelog.js';
  import { APP_VERSION } from '#lib/stores/appVersion.js';
  import { fmt } from '#lib/format/ru.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { theme, type ThemePref } from '#lib/theme.js';
  import { getEnterPref, setEnterPref, type EnterPref } from '#lib/composer/prefs.js';
  import { currentUpdate, installation } from '#lib/pwa/register.js';
  import { staleness } from '#lib/pwa/sync-state.js';
  import { purgeLocal } from '#lib/pwa/signout.js';
  import { session } from '#lib/api/http.js';
  import { authApi } from '#lib/auth/api.js';
  import { pane } from '#lib/shell/pane.svelte.js';
  import TopBar from '#lib/ui/TopBar.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import PalettePicker from './PalettePicker.svelte';
  import SpawnWhitelist from './SpawnWhitelist.svelte';
  import LimitSettings from './LimitSettings.svelte';
  import PromptSuggestionSetting from './PromptSuggestionSetting.svelte';
  import DictationSettings from './DictationSettings.svelte';
  import RoutingScreen from './RoutingScreen.svelte';
  import SignInSettings from '../signin/SignInSettings.svelte';
  import { SETTINGS_CATEGORIES, settingsCategory, type SettingsCategory } from './settingsNav.js';
  import { motionPref, reasoningPref, saveMotion, saveReasoningPref, type MotionPref, type ReasoningPref } from './preferences.js';
  import '../secondary.css';

  // Стол: список разделов слева и раздел справа. Телефон и планшет: список разделов, раздел открывается поверх.
  const desktop = $derived(pane.mode === 'desktop');
  const category = $derived(settingsCategory(page.url.searchParams.get('c'), pane.mode));
  const categoryTitle = $derived(SETTINGS_CATEGORIES.find((entry) => entry.id === category)?.title ?? 'Настройки');

  let ready = $state(false);
  let runtime = $state<Runtime | null>(null); let config = $state<components['schemas']['ConfigView'] | null>(null);
  let login = $state(''); let error = $state(''); let busy = $state(false); let purged = $state(false); let logoutFailed = $state(false);
  let enter = $state('auto'); let motion = $state('normal'); let reasoning = $state<ReasoningPref>('live');
  let canInstall = $state(false);
  const update = $derived.by(() => { void ticker.second; return currentUpdate(); });
  const updateState = $derived.by(() => { void ticker.second; return update?.state ?? 'idle'; });
  const hubVersion = $derived(config?.version ?? null);
  const versionMismatch = $derived(hubVersion !== null && compareVersions(APP_VERSION, hubVersion) !== 0);
  const sync = $derived.by(() => { void ticker.second; return staleness.lastSyncAt; });
  const phase = $derived(runtime?.connection.status.phase);
  const connectionText = $derived(phase === 'live' ? 'Подключено' : phase === 'auth_required' ? 'Требуется вход' : phase === 'incompatible' ? 'Требуется обновление' : phase === 'connecting' ? 'Подключаюсь' : 'Нет связи');
  const themeLabel = $derived(theme.pref === 'dark' ? 'Тёмный' : theme.pref === 'light' ? 'Светлый' : 'Как в системе');
  $effect(() => { if (ready && !purged) saveMotion(motion as MotionPref); });
  $effect(() => { if (ready && !purged) setEnterPref(enter as EnterPref); });
  // Рассуждения сохраняются на самом выборе: загрузка экрана ничего не перезаписывает.
  function setReasoning(value: string): void {
    reasoning = value as ReasoningPref;
    saveReasoningPref(reasoning);
  }
  async function load() {
    error = '';
    try { const [rt, cfg, who] = await Promise.all([browserRuntime(), loadConfig(), me()]); runtime = rt; config = cfg; login = who.data.login; }
    catch { error = 'Не удалось загрузить настройки. Повторите при восстановлении связи'; }
  }
  onMount(() => {
    enter = getEnterPref(); motion = motionPref(); reasoning = reasoningPref(); ready = true;
    const install = installation(); const changed = () => canInstall = install.canInstall;
    changed(); install.addEventListener('change', changed); void load();
    const releaseTicker = ticker.retain();
    return () => { releaseTicker(); install.removeEventListener('change', changed); };
  });
  /** Выход: сначала сеанс на хабе (HttpOnly-cookie снимает только ответ хаба), затем данные устройства в любом случае. */
  async function clear(exit: boolean) {
    if (busy || !runtime) return;
    busy = true; error = ''; logoutFailed = false;
    let closed = !exit;
    if (exit) {
      try { await authApi.logout(); closed = true; }
      catch { logoutFailed = true; }
    }
    try {
      await purgeLocal({ idb: runtime.db, caches, ls: localStorage, connection: runtime.connection, runtime });
      purged = true;
      if (exit && closed) session.expire('session_revoked', 'logout');
    } catch { error = 'Не удалось очистить данные на устройстве. Повторите очистку'; }
    finally { busy = false; }
  }
  async function updateAction() {
    try { if (updateState === 'idle') await update?.check(); else { await runtime?.drafts.flush(); await update?.apply(); } }
    catch { error = 'Не удалось обновить приложение. Повторите при восстановлении связи'; }
  }
  const href = (id: SettingsCategory) => `${resolve('/settings')}?c=${id}`;
  const hint = (id: SettingsCategory, text: string) => id === 'appearance' ? `${themeLabel}, тема, анимации` : text;
  const busyProps = $derived(busy ? { disabled: true as const, reason: 'Очистка уже выполняется' } : { disabled: false as const });
</script>

<main id="main" tabindex="-1" class="settings" class:desktop data-settings={category ?? 'index'} data-secondary-ready={config ? '' : undefined}>
  {#if !desktop}
    <TopBar title={category ? categoryTitle : 'Настройки'}>
      {#snippet leading()}
        <a class="back" href={category ? resolve('/settings') : resolve('/sessions')} aria-label={category ? 'К настройкам' : 'К сессиям'}><Icon name="back" /></a>
      {/snippet}
      {#snippet actions()}<IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}
    </TopBar>
  {/if}
  <div class="frame">
    {#if desktop}
      <!-- Шапка стола делит колонки с разделами: заголовок над списком, название раздела над ним самим. -->
      <h1 class="frame-title">Настройки</h1>
      <div class="pane-head"><h2>{categoryTitle}</h2><IconButton icon={theme.effective === 'dark' ? 'sun' : 'moon'} label="Сменить тему" onclick={() => theme.toggle()} /></div>
    {/if}
    {#if desktop || !category}
      <div class="index">
        <nav class="cats" aria-label="Разделы настроек">
          {#each SETTINGS_CATEGORIES as entry (entry.id)}
            <!-- Адрес собран из resolve('/settings') и раздела: правило не видит resolve внутри шаблонной строки. -->
            <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
            <a class="cat" href={href(entry.id)} aria-current={category === entry.id ? 'page' : undefined} data-category={entry.id}>
              <span class="cat-text"><span class="cat-title">{entry.title}</span><span class="cat-hint">{hint(entry.id, entry.hint)}</span></span>
              {#if !desktop}<span class="chevron" aria-hidden="true"><Icon name="chevron" /></span>{/if}
            </a>
          {/each}
        </nav>
        {#if !desktop}<p class="foot">{[`Canterlot ${APP_VERSION}`, hubVersion ? `хаб ${hubVersion}` : '', login].filter(Boolean).join(' · ')}</p>{/if}
      </div>
    {/if}
    {#if category}
      <div class="pane secondary-content" data-settings-pane={category}>
        {#if error}<p class="error" role="alert">{error}</p>{#if !runtime}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
        {#if purged}<p role="status">Данные на устройстве удалены</p>{#if logoutFailed}<p class="error" role="alert">Данные удалены, но сеанс на сервере не завершён: завершите его с другого устройства</p>{/if}
        {:else if !config}<p>Загружаю настройки…</p>
        {:else if category === 'appearance'}
          <div class="group">
            <section class="setting"><h3>Режим</h3><Segmented name="theme" label="Режим" options={[{ value: 'dark', label: 'Тёмный' }, { value: 'light', label: 'Светлый' }, { value: 'system', label: 'Как в системе' }]} bind:value={() => theme.pref, (value) => theme.set(value as ThemePref)} /></section>
            <section class="setting"><h3>Тема</h3><PalettePicker /></section>
            <section class="setting"><h3>Скорость анимаций</h3><p class="note">Появление текста, дорожки этапов и раскрытие блоков. Системное уменьшение движения имеет приоритет</p><Segmented name="motion" label="Скорость анимаций" options={[{ value: 'off', label: 'Выкл' }, { value: 'slower', label: 'Медленнее' }, { value: 'normal', label: 'Обычно' }, { value: 'faster', label: 'Быстрее' }]} bind:value={motion} /></section>
          </div>
        {:else if category === 'input'}
          <h3 class="group-title">Поле ввода</h3>
          <div class="group">
            <section class="setting"><h3>Enter на клавиатуре</h3><p class="note">Авто: на телефоне новая строка, на компьютере отправка. Shift+Enter добавляет строку</p><Segmented name="enter" label="Enter на клавиатуре" options={[{ value: 'auto', label: 'Авто' }, { value: 'send', label: 'Отправка' }, { value: 'newline', label: 'Новая строка' }]} bind:value={enter} /></section>
            <div class="setting embedded"><PromptSuggestionSetting /></div>
          </div>
          <DictationSettings />
          <h3 class="group-title">Лента</h3>
          <div class="group">
            <section class="setting"><h3>Рассуждения</h3><p class="note">Вживую показывает хвост мысли, пока агент думает, и складывает его в строку после ответа. Строкой оставляет начало мысли, Раскрыты показывает рассуждение целиком</p><Segmented name="reasoning" label="Рассуждения" options={[{ value: 'line', label: 'Строкой' }, { value: 'live', label: 'Вживую' }, { value: 'expanded', label: 'Раскрыты' }]} bind:value={() => reasoning, setReasoning} /></section>
          </div>
        {:else if category === 'limits'}
          <LimitSettings />
          <a class="link-row" href={resolve('/limits')}><span>История лимитов</span><Icon name="chevron" /></a>
          <SpawnWhitelist routing={href('routing')} />
        {:else if category === 'routing'}
          <div class="embed"><RoutingScreen embedded /></div>
        {:else if category === 'security'}
          <h3 class="group-title">Подключение</h3>
          <div class="group">
            <dl class="facts">
              <dt>Логин</dt><dd><strong>{login}</strong></dd>
              <dt>Связь с хабом</dt><dd data-tone={phase === 'live' ? 'ok' : 'warn'}>{connectionText}</dd>
              <dt>Последняя синхронизация</dt><dd>{sync ? fmt.clock(sync) : 'ещё не было'}</dd>
            </dl>
          </div>
          <div class="embed"><SignInSettings embedded /></div>
          <h3 class="group-title">Данные и выход</h3>
          <div class="group">
            <div class="exit">
              <Button {...busyProps} onclick={() => clear(false)}>Очистить данные на устройстве</Button>
              <div class="logout"><Button {...busyProps} onclick={() => clear(true)}>Выйти</Button></div>
            </div>
            {#if busy}<p role="status">Очищаю данные на устройстве…</p>{/if}
            <p class="note">Выход завершает этот сеанс на хабе и удаляет данные на устройстве</p>
          </div>
        {:else if category === 'app'}
          <div class="group">
            <dl class="facts">
              <dt>Приложение</dt><dd><strong>{APP_VERSION}</strong></dd>
              <dt>Хаб</dt><dd><strong>{hubVersion ?? 'нет данных'}</strong></dd>
              <dt>Обновление</dt><dd>{updateState === 'idle' ? 'установлена текущая версия' : updateState === 'applying' ? 'обновляю…' : 'доступна новая версия'}</dd>
            </dl>
            {#if versionMismatch}<p class="note">Версии приложения и хаба различаются. Обновите отставшую часть при удобном случае</p>{/if}
            <div class="buttons">
              {#if update}<Button onclick={updateAction}>{updateState === 'idle' ? 'Проверить обновления' : 'Обновить приложение'}</Button>{/if}
              {#if canInstall}<Button onclick={() => { void installation().prompt().catch(() => error = 'Не удалось установить приложение. Повторите в меню браузера'); }}>Установить приложение</Button>{/if}
            </div>
          </div>
          <a class="link-row" href={resolve('/changes')}><span>История изменений</span><Icon name="chevron" /></a>
          <a class="link-row" href={resolve('/machines')}><span>Машины</span><Icon name="chevron" /></a>
        {/if}
      </div>
    {/if}
  </div>
</main>

<style>
  .settings { --settings-bg: var(--bg); display: flex; flex-direction: column; min-height: 0; }
  .settings.desktop { --settings-bg: var(--chat-bg); }
  .back { min-height: var(--target); min-width: var(--target); display: flex; align-items: center; justify-content: center; padding: var(--s4); color: var(--text); }
  .frame { flex: 1; min-height: 0; overflow: auto; padding: var(--s4) var(--gutter) var(--s12); }
  .settings:not(.desktop) .frame > * { max-width: var(--reading-max); margin-inline: auto; }
  /* Стол: две колонки под общей шапкой; ширина раздела — как у чтения, список разделов узкий. */
  .desktop .frame { display: grid; grid-template-columns: var(--settings-nav) minmax(0, var(--reading-max)); grid-template-rows: auto 1fr; align-items: start; justify-content: center; column-gap: var(--s12); padding: var(--s8) var(--s10) var(--s12); }
  /* Обе половины шапки в одном ряду сетки и по центру его высоты: заголовки на одной линии. */
  .frame-title, .pane-head { align-self: center; }
  .frame-title { margin: var(--s0) var(--s0) var(--s6); padding: var(--s0) var(--s6); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .pane-head { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); min-height: var(--target); margin-bottom: var(--s6); }
  .pane-head h2 { margin: var(--s0); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .desktop .index { position: sticky; top: var(--s0); }
  .cats { display: flex; flex-direction: column; gap: var(--s1); }
  .cat { display: flex; align-items: center; gap: var(--s5); min-height: var(--target); padding: var(--s4) var(--s6); border-radius: var(--radius-sm); color: var(--text); text-decoration: none; }
  .cat-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: var(--s1); }
  .cat-title { font-weight: var(--weight-semibold); }
  .cat-hint { overflow: hidden; color: var(--muted); font-size: var(--fs-sm); text-overflow: ellipsis; white-space: nowrap; }
  .chevron { display: inline-flex; color: var(--muted); }
  .cat[aria-current='page'] { background: var(--selected); }
  .cat[aria-current='page'] .cat-title { color: var(--accent-link); }
  @media (hover: hover) and (pointer: fine) { .cat:not([aria-current='page']):hover { background: var(--hover); } }
  .cat:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: calc(var(--focus-width) * -1); }
  /* Телефон: разделы — одна поверхность со строками, как список системных настроек. */
  .settings:not(.desktop) .cats { padding: var(--s2); border-radius: var(--radius-card); background: var(--surface); }
  .foot { margin: var(--s6) var(--s6) var(--s2); color: var(--muted); font-size: var(--fs-sm); }
  .pane { margin-block: var(--s0); }
  .desktop .pane { max-width: none; margin: var(--s0); }
  .group-title { margin: var(--s10) var(--s6) var(--s4); color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .group-title:first-child { margin-top: var(--s0); }
  /* Группа — одна поверхность, настройки внутри разделены линией, а не отдельными карточками. */
  .group { margin-bottom: var(--s6); padding: var(--s2) var(--s8); border-radius: var(--radius-card); background: var(--surface); }
  .setting { display: flex; flex-direction: column; gap: var(--s4); margin: var(--s0); padding: var(--s7) var(--s0); }
  .setting + .setting { border-top: var(--hairline) solid var(--line); }
  .setting h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .setting .note { margin: var(--s0); }
  .embedded :global(section) { margin: var(--s0); }
  .embedded :global(h2) { margin: var(--s0) var(--s0) var(--s4); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .embed :global(.secondary-body) { overflow: visible; padding: var(--s0); }
  .embed :global(.secondary-content) { max-width: none; }
  .embed :global(.secondary-content > h2:first-child) { margin-top: var(--s0); }
  .link-row { display: flex; align-items: center; justify-content: space-between; min-height: var(--target); margin-bottom: var(--s6); padding: var(--s4) var(--s8); border-radius: var(--radius-card); background: var(--surface); text-decoration: none; }
  .secondary-content .link-row { color: var(--text); }
  .link-row :global(svg) { color: var(--muted); }
  @media (hover: hover) and (pointer: fine) { .link-row:hover { background: var(--hover); } }
  .facts { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: var(--s4) var(--s10); margin: var(--s0); padding: var(--s6) var(--s0); }
  .facts dt { color: var(--muted); }
  .facts dd { margin: var(--s0); }
  .facts dd[data-tone='ok'] { color: var(--success); }
  .facts dd[data-tone='warn'] { color: var(--warning); }
  /* Обе кнопки выхода одной ширины: рядом на столе, друг под другом на узком экране. */
  .exit { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--settings-button)), 1fr)); gap: var(--s4); padding-top: var(--s6); }
  .exit :global(button) { width: 100%; }
  .logout :global(button) { color: var(--danger); }
  .buttons { display: flex; flex-wrap: wrap; gap: var(--s4); padding-bottom: var(--s6); }
</style>
