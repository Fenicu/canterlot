<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { loadConfig } from '#lib/api/config.js';
  import { me } from '#lib/api/client.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { fmt } from '#lib/format/ru.js';
  import { theme, type ThemePref } from '#lib/theme.js';
  import { getEnterPref, setEnterPref, type EnterPref } from '#lib/composer/prefs.js';
  import { currentUpdate, installation } from '#lib/pwa/register.js';
  import { staleness } from '#lib/pwa/offline.js';
  import { purgeLocal } from '#lib/pwa/signout.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import SpawnWhitelist from './SpawnWhitelist.svelte';
  import { motionPref, saveMotion, type MotionPref } from './preferences.js';
  import '../secondary.css';

  let ready = $state(false);
  let runtime = $state<Runtime | null>(null); let config = $state<components['schemas']['ConfigView'] | null>(null);
  let login = $state(''); let error = $state(''); let busy = $state(false); let purged = $state(false);
  let enter = $state('auto'); let motion = $state('normal');
  let canInstall = $state(false); let tick = $state(0);
  const update = $derived.by(() => { void tick; return currentUpdate(); });
  const updateState = $derived.by(() => { void tick; return update?.state ?? 'idle'; });
  const sync = $derived.by(() => { void tick; return staleness.lastSyncAt; });
  const phase = $derived(runtime?.connection.status.phase);
  const connectionText = $derived(phase === 'live' ? 'Подключено' : phase === 'auth_required' ? 'Требуется вход' : phase === 'incompatible' ? 'Требуется обновление' : phase === 'connecting' ? 'Подключаюсь' : 'Нет связи');
  $effect(() => { if (ready && !purged) saveMotion(motion as MotionPref); });
  $effect(() => { if (ready && !purged) setEnterPref(enter as EnterPref); });
  async function load() {
    error = '';
    try { const [rt, cfg, who] = await Promise.all([browserRuntime(), loadConfig(), me()]); runtime = rt; config = cfg; login = who.data.login; }
    catch { error = 'Не удалось загрузить настройки. Повторите при восстановлении связи'; }
  }
  onMount(() => {
    enter = getEnterPref(); motion = motionPref(); ready = true;
    const install = installation(); const changed = () => canInstall = install.canInstall;
    changed(); install.addEventListener('change', changed); void load();
    const timer = setInterval(() => tick++, 1000);
    return () => { clearInterval(timer); install.removeEventListener('change', changed); };
  });
  async function clear(exit: boolean) {
    if (busy || !runtime || !config) return;
    busy = true; error = '';
    const logout = config.auth.logout_url;
    try {
      await purgeLocal({ idb: runtime.db, caches, ls: localStorage, connection: runtime.connection, runtime });
      purged = true;
      if (exit && logout) window.location.assign(logout);
    } catch { error = 'Не удалось очистить данные на устройстве. Повторите очистку'; }
    finally { busy = false; }
  }
  async function updateAction() {
    try { if (updateState === 'idle') await update?.check(); else { await runtime?.drafts.flush(); await update?.apply(); } }
    catch { error = 'Не удалось обновить приложение. Повторите при восстановлении связи'; }
  }
</script>
<main id="main" tabindex="-1" data-secondary-ready={config ? '' : undefined}>
  <SecondaryHeader title="Настройки" />
  <div class="secondary-body"><div class="secondary-content">
    {#if error}<p class="error" role="alert">{error}</p>{#if !runtime}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
    {#if purged}<p role="status">Данные на устройстве удалены</p><p>Сессия Authelia завершается на собственной странице. Откройте страницу Authelia, чтобы завершить вход</p>
    {:else if !config}<p>Загружаю настройки…</p>
    {:else}
      <div class="settings-grid"><div>
        <section><h2>Тема</h2><Segmented name="theme" label="Тема" options={[{ value: 'dark', label: 'Тёмная' }, { value: 'light', label: 'Светлая' }, { value: 'system', label: 'Как в системе' }]} bind:value={() => theme.pref, (value) => theme.set(value as ThemePref)} />

        </section>
        <section><h2>Скорость анимаций</h2><Segmented name="motion" label="Скорость анимаций" options={[{ value: 'off', label: 'Выкл' }, { value: 'slower', label: 'Медленнее' }, { value: 'normal', label: 'Обычно' }, { value: 'faster', label: 'Быстрее' }]} bind:value={motion} /><p class="note">Появление текста, дорожки этапов и раскрытие блоков. Системное уменьшение движения имеет приоритет</p></section>
        <section><h2>Enter на клавиатуре</h2><Segmented name="enter" label="Enter на клавиатуре" options={[{ value: 'auto', label: 'Авто' }, { value: 'send', label: 'Отправка' }, { value: 'newline', label: 'Новая строка' }]} bind:value={enter} /><p class="note">Авто: на телефоне новая строка, на компьютере отправка. Shift+Enter добавляет строку</p></section>
        <section><h2>Данные и выход</h2><Button {...(busy ? { disabled: true as const, reason: 'Очистка уже выполняется' } : { disabled: false as const })} onclick={() => clear(false)}>Очистить данные на устройстве</Button><div class="logout"><Button full {...(busy ? { disabled: true as const, reason: 'Очистка уже выполняется' } : { disabled: false as const })} onclick={() => clear(true)}>Выйти</Button></div>{#if busy}<p role="status">Очищаю данные на устройстве…</p>{/if}<p class="note">{config.auth.logout_url ? 'Перед выходом через Authelia данные на устройстве будут удалены' : 'Сессия Authelia завершается на собственной странице'}</p></section>
      </div><div>
        <section><h2>Подключение</h2><p>Логин: <strong>{login}</strong></p><p>{connectionText}</p>{#if sync}<p class="note">Последняя синхронизация: {fmt.clock(sync)}</p>{/if}</section>
        <section><h2>Предупреждения о лимитах</h2><div class="card"><p>Предупредить при {config.limit_thresholds.warn_pcts.join('%, ')}%</p><p>Резерв владельца {config.limit_thresholds.weekly_reserve_pct}%</p><p class="note">Для 5 часов и недели, Claude и Codex. Кранч ждёт сброса, сохраняя резерв владельца</p></div><a href={resolve('/limits')}>История лимитов</a></section>
        <section><a href={resolve('/machines')}>Машины</a></section>
        <SpawnWhitelist />
        <section><h2>О приложении</h2><p>canterlot</p>{#if canInstall}<button type="button" onclick={() => { void installation().prompt().catch(() => error = 'Не удалось установить приложение. Повторите в меню браузера'); }}>Установить приложение</button>{/if}<p>Обновление: {updateState === 'idle' ? 'Установлена текущая версия' : updateState === 'applying' ? 'Обновляю…' : 'Доступна новая версия'}</p>{#if update}<button type="button" onclick={updateAction}>{updateState === 'idle' ? 'Проверить обновления' : 'Обновить приложение'}</button>{/if}</section>
      </div></div>
    {/if}
  </div></div>
</main>

<style>
  section { margin-bottom: var(--s10); }
  .card { padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  section > a { display: inline-flex; align-items: center; min-height: var(--target); }
  .logout { display: block; margin-top: var(--s6); color: var(--danger); width: 100%; }
  @media (min-width: 1100px) { .settings-grid { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s10); } }
</style>
