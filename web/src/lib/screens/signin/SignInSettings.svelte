<script lang="ts">
  import { onMount, type Component } from 'svelte';
  import { page } from '$app/state';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { fmt } from '#lib/format/ru.js';
  import { me } from '#lib/api/client.js';
  import { authApi, type MethodsView, type OwnerSession, type OwnerSessions, type Passkey } from '#lib/auth/api.js';
  import { cascadeOf, cascadeText, IDLE_OPTIONS, ipText, methodText, placeText, seenText, sessionTitle } from '#lib/auth/sessions.js';
  import { signInErrorText } from '#lib/auth/signin.js';
  import { stepUp, StepUpCancelled } from '#lib/auth/stepup.svelte.js';
  import { createCredential, webauthnAvailable, webauthnErrorText } from '#lib/auth/webauthn.js';
  import StepUpPrompt from '#lib/auth/StepUpPrompt.svelte';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import '../secondary.css';

  let view = $state<OwnerSessions | null>(null);
  let keys = $state<Passkey[]>([]);
  let methods = $state<MethodsView>({ password: false, passkey: false });
  let loadError = $state('');
  let error = $state('');
  let notice = $state('');
  let selectedId = $state<string | null>(null);
  let renaming = $state('');
  let renameOpen = $state(false);
  let confirmTerminate = $state<OwnerSession | null>(null);
  let confirmOthers = $state(false);
  let keyRename = $state<Passkey | null>(null);
  let keyName = $state('');
  let keyDelete = $state<Passkey | null>(null);
  let passwordOpen = $state(false);
  let currentPassword = $state('');
  let newPassword = $state('');
  let repeatPassword = $state('');
  let passwordError = $state('');
  let idle = $state('90');
  let login = $state('');
  let scanning = $state(false);
  let secret = $state('');
  let Scanner = $state<Component<{ onsecret: (secret: string) => void; oncancel: () => void }> | null>(null);
  let Approve = $state<Component<{ secret: string }> | null>(null);
  let recovered = $state(page.url.searchParams.get('from') === 'recovery');

  const now = $derived.by(() => { void ticker.second; return clock.serverNow(); });
  const sessions = $derived(view?.sessions ?? []);
  const current = $derived(sessions.find((session) => session.current) ?? null);
  const others = $derived(sessions.filter((session) => !session.current));
  const selected = $derived(sessions.find((session) => session.id === selectedId) ?? null);
  const canPasskey = $derived(webauthnAvailable());

  async function load(): Promise<void> {
    try {
      const [list, passkeys, available, who] = await Promise.all([authApi.sessions(), authApi.passkeys(), authApi.methods(), me()]);
      login = who.data.login; view = list; keys = passkeys.passkeys; methods = available; idle = String(list.idle_timeout_days); loadError = '';
    } catch (err) { loadError = signInErrorText(err, 'Не удалось загрузить сеансы'); }
  }

  onMount(() => {
    void load();
    const release = ticker.retain();
    const visible = () => { if (!document.hidden) void load(); };
    document.addEventListener('visibilitychange', visible);
    return () => { release(); document.removeEventListener('visibilitychange', visible); };
  });

  /** Действие с подтверждением входа: отмена окна молча, остальные ошибки строкой над списком. */
  async function act(work: () => Promise<unknown>, done?: string): Promise<boolean> {
    error = ''; notice = '';
    try {
      await stepUp.run(work);
      if (done) notice = done;
      await load();
      return true;
    } catch (err) {
      if (!(err instanceof StepUpCancelled)) error = err instanceof DOMException ? webauthnErrorText(err) : signInErrorText(err, 'Не удалось выполнить действие');
      return false;
    }
  }

  function subtitle(session: OwnerSession): string {
    const parts = [session.name ? session.label : null, ipText(session), placeText(session.location), session.current ? 'этот сеанс' : seenText(session.last_seen_at, now)];
    return parts.filter(Boolean).join(' · ');
  }

  async function rename(): Promise<void> {
    const target = selected;
    if (!target) return;
    renameOpen = false;
    await act(() => authApi.renameSession(target.id, renaming.trim()), 'Имя сеанса сохранено');
  }

  async function terminate(target: OwnerSession): Promise<void> {
    selectedId = null; confirmTerminate = null;
    await act(() => authApi.deleteSession(target.id), 'Сеанс завершён');
  }

  function setIdle(value: string): void {
    idle = value;
    void act(() => authApi.settings(Number(value)), 'Срок бездействия сохранён');
  }

  async function addPasskey(): Promise<void> {
    await act(async () => {
      const challenge = await authApi.registerStart();
      const credential = await createCredential(challenge.options);
      await authApi.registerFinish(challenge.challenge_id, credential);
    }, 'Ключ доступа добавлен');
  }

  async function savePassword(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    passwordError = '';
    const bytes = new TextEncoder().encode(newPassword).length;
    if (bytes < 10) { passwordError = 'Пароль должен быть не короче 10 символов'; return; }
    if (bytes > 1024) { passwordError = 'Пароль слишком длинный'; return; }
    if (newPassword !== repeatPassword) { passwordError = 'Пароли не совпадают'; return; }
    try {
      await stepUp.run(() => authApi.setPassword(newPassword, methods.password ? currentPassword : undefined));
      passwordOpen = false; currentPassword = ''; newPassword = ''; repeatPassword = '';
      notice = methods.password ? 'Пароль изменён. Остальные сеансы завершены' : 'Пароль задан. Остальные сеансы завершены';
      await load();
    } catch (err) {
      if (!(err instanceof StepUpCancelled)) passwordError = signInErrorText(err, 'Не удалось сохранить пароль');
    }
  }

  function startScan(): void {
    error = ''; secret = ''; scanning = true;
    if (!Scanner) void import('#lib/auth/QrScanner.svelte').then((m) => { Scanner = m.default; }, () => { scanning = false; error = 'Не удалось открыть сканер. Обновите страницу'; });
    if (!Approve) void import('#lib/auth/QrApprove.svelte').then((m) => { Approve = m.default; });
  }

  function dismissRecovery(): void {
    recovered = false;
    history.replaceState(history.state, '', location.pathname);
  }
</script>

<main id="main" tabindex="-1" data-secondary-ready={view ? '' : undefined} data-sign-in-settings>
  <SecondaryHeader title="Сеансы и вход" back={{ path: '/settings', label: 'К настройкам' }} />
  <div class="secondary-body"><div class="secondary-content">
    {#if recovered}
      <div class="banner" role="status" data-recovery-banner>
        <p><strong>Вы вошли по ссылке восстановления.</strong> Задайте пароль или добавьте ключ доступа, чтобы входить без ссылки, и завершите сеансы, которых не узнаёте</p>
        <Button variant="text" onclick={dismissRecovery}>Понятно</Button>
      </div>
    {/if}
    {#if loadError}<p class="error" role="alert">{loadError}</p><button type="button" onclick={() => void load()}>Повторить</button>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if notice}<p class="ok" role="status">{notice}</p>{/if}

    {#if scanning}
      <section class="panel">
        {#if secret && Approve}<Approve {secret} /><Button variant="text" onclick={() => { scanning = false; secret = ''; }}>Закрыть</Button>
        {:else if Scanner}<Scanner onsecret={(value) => { secret = value; }} oncancel={() => { scanning = false; }} />
        {:else}<p role="status">Открываю сканер…</p>{/if}
      </section>
    {/if}

    {#if view}
      <section aria-labelledby="sessions-title">
        <h2 id="sessions-title">Сеансы</h2>
        <ul class="rows">
          {#each [current, ...others].filter((item): item is OwnerSession => item !== null) as session (session.id)}
            <li>
              <button type="button" class="row" data-session={session.id} onclick={() => { selectedId = session.id; renaming = session.name ?? ''; }}>
                <span class="title">{sessionTitle(session)}{#if session.current}<Pill color="accent">этот сеанс</Pill>{/if}</span>
                <span class="sub">{subtitle(session)}</span>
                <span class="sub">{methodText(session.method)}</span>
              </button>
            </li>
          {/each}
        </ul>
        {#if others.length}<Button full onclick={() => { confirmOthers = true; }}>Завершить все остальные сеансы</Button>{/if}
        <div class="gap"><Button full onclick={startScan}>Сканировать код</Button></div>
      </section>

      <section>
        <h2>Автоматически завершать сеанс, если он неактивен</h2>
        <Segmented name="idle" label="Срок бездействия" options={IDLE_OPTIONS} bind:value={() => idle, setIdle} />
      </section>

      <section aria-labelledby="passkeys-title">
        <h2 id="passkeys-title">Ключи доступа</h2>
        {#if keys.length === 0}<p class="note">Ключей доступа пока нет</p>{/if}
        <ul class="rows">
          {#each keys as key (key.id)}
            <li class="row static" data-passkey={key.id}>
              <span class="title">{key.name}</span>
              <span class="sub">добавлен {fmt.stamp(Date.parse(key.created_at), now)} · {key.last_used_at ? `использован ${fmt.stamp(Date.parse(key.last_used_at), now)}` : 'ещё не использован'}</span>
              <span class="actions">
                <Button variant="text" label={`Переименовать ключ ${key.name}`} onclick={() => { keyRename = key; keyName = key.name; }}>Переименовать</Button>
                <Button variant="text" label={`Удалить ключ ${key.name}`} onclick={() => { keyDelete = key; }}>Удалить</Button>
              </span>
            </li>
          {/each}
        </ul>
        {#if canPasskey}<Button full onclick={() => void addPasskey()}>Добавить ключ доступа</Button>
        {:else}<p class="note">Этот браузер не поддерживает ключи доступа</p>{/if}
      </section>

      <section aria-labelledby="password-title">
        <h2 id="password-title">Пароль</h2>
        {#if passwordOpen}
          <form class="password" onsubmit={savePassword}>
            <input class="hidden-login" name="username" autocomplete="username" value={login} readonly tabindex="-1" aria-hidden="true" />
            {#if methods.password}<Field label="Текущий пароль">{#snippet children({ id })}<input {id} type="password" autocomplete="current-password" bind:value={currentPassword} />{/snippet}</Field>{/if}
            <Field label="Новый пароль" hint="Не короче 10 символов">{#snippet children({ id, describedby })}<input {id} type="password" autocomplete="new-password" aria-describedby={describedby} bind:value={newPassword} />{/snippet}</Field>
            <Field label="Повторите пароль" error={passwordError || undefined}>{#snippet children({ id, describedby, invalid })}<input {id} type="password" autocomplete="new-password" aria-describedby={describedby} aria-invalid={invalid} bind:value={repeatPassword} />{/snippet}</Field>
            <p class="note">После смены пароля все остальные сеансы будут завершены</p>
            <div class="actions"><Button type="submit" variant="primary">Сохранить пароль</Button><Button variant="text" onclick={() => { passwordOpen = false; passwordError = ''; }}>Отмена</Button></div>
          </form>
        {:else}
          <p class="note">{methods.password ? 'Пароль задан' : 'Пароль не задан'}</p>
          <Button full onclick={() => { passwordOpen = true; }}>{methods.password ? 'Сменить пароль' : 'Задать пароль'}</Button>
        {/if}
      </section>
    {:else if !loadError}<p role="status">Загружаю сеансы…</p>{/if}

    <p class="note attribution"><a href="https://db-ip.com" rel="noopener noreferrer" target="_blank">IP-геолокация: DB-IP</a></p>
  </div></div>
</main>

<Sheet open={selected !== null} title={selected ? sessionTitle(selected) : ''} routeKey="session-details" onclose={() => { selectedId = null; renameOpen = false; }}>
  {#if selected}
    <dl class="details" data-session-details>
      {#if selected.label}<div><dt>Устройство</dt><dd>{selected.label}</dd></div>{/if}
      <div><dt>Вход</dt><dd>{methodText(selected.method)}, {fmt.stamp(Date.parse(selected.created_at), now)}</dd></div>
      <div><dt>Активность</dt><dd>{selected.current ? 'этот сеанс' : seenText(selected.last_seen_at, now)}</dd></div>
      <div><dt>IP-адрес</dt><dd>{ipText(selected)}{#if selected.created_ip && selected.last_ip && selected.created_ip !== selected.last_ip}, при входе {selected.created_ip}{/if}</dd></div>
      {#if placeText(selected.location)}<div><dt>Место</dt><dd>{placeText(selected.location)}</dd></div>{/if}
      {#if selected.approved_by}<div><dt>Вход подтвердил</dt><dd>{sessions.find((item) => item.id === selected.approved_by) ? sessionTitle(sessions.find((item) => item.id === selected.approved_by)!) : 'сеанс, который уже завершён'}</dd></div>{/if}
      {#if selected.approved.length}<div><dt>Подтвердил вход</dt><dd>{selected.approved.map((id) => { const found = sessions.find((item) => item.id === id); return found ? sessionTitle(found) : 'завершённый сеанс'; }).join(', ')}</dd></div>{/if}
      {#if selected.user_agent}<div><dt>User agent</dt><dd class="ua">{selected.user_agent}</dd></div>{/if}
    </dl>
    {#if renameOpen}
      <form class="rename" onsubmit={(event) => { event.preventDefault(); void rename(); }}>
        <Field label="Имя сеанса" hint="Пустое имя вернёт подпись устройства">{#snippet children({ id, describedby })}<input {id} maxlength="64" aria-describedby={describedby} bind:value={renaming} />{/snippet}</Field>
        <div class="actions"><Button type="submit" variant="primary">Сохранить</Button><Button variant="text" onclick={() => { renameOpen = false; }}>Отмена</Button></div>
      </form>
    {:else}
      <div class="actions">
        <Button onclick={() => { renameOpen = true; }}>Переименовать</Button>
        {#if !selected.current}<span class="danger"><Button onclick={() => { confirmTerminate = selected; }}>Завершить сеанс</Button></span>{/if}
      </div>
      {#if selected.current}<p class="note">Чтобы выйти на этом устройстве, нажмите «Выйти» в настройках</p>{/if}
    {/if}
  {/if}
</Sheet>

{#if confirmTerminate}
  {@const target = confirmTerminate}
  <ConfirmDialog title="Завершить сеанс?" body={[`Сеанс «${sessionTitle(target)}» будет завершён, данные на том устройстве удалятся.`, cascadeText(cascadeOf(target.id, sessions))].filter(Boolean).join(' ')} confirmLabel="Завершить" danger onconfirm={() => void terminate(target)} onclose={() => { confirmTerminate = null; }} />
{/if}
{#if confirmOthers}
  <ConfirmDialog title="Завершить все остальные сеансы?" body={[`Будут завершены все сеансы, кроме этого: ${others.length}.`, current ? cascadeText(cascadeOf(current.id, sessions)) : ''].filter(Boolean).join(' ')} confirmLabel="Завершить" danger onconfirm={() => { confirmOthers = false; void act(() => authApi.revokeOthers(), 'Остальные сеансы завершены'); }} onclose={() => { confirmOthers = false; }} />
{/if}
{#if keyDelete}
  {@const key = keyDelete}
  <ConfirmDialog title="Удалить ключ доступа?" body={`Ключ «${key.name}» больше не подойдёт для входа в canterlot.`} confirmLabel="Удалить" danger onconfirm={() => { const id = key.id; keyDelete = null; void act(() => authApi.deletePasskey(id), 'Ключ доступа удалён'); }} onclose={() => { keyDelete = null; }} />
{/if}
<Sheet open={keyRename !== null} title="Имя ключа доступа" routeKey="passkey-rename" onclose={() => { keyRename = null; }}>
  <form class="rename" onsubmit={(event) => { event.preventDefault(); const key = keyRename; keyRename = null; if (key) void act(() => authApi.renamePasskey(key.id, keyName.trim()), 'Имя ключа сохранено'); }}>
    <Field label="Имя ключа">{#snippet children({ id })}<input {id} maxlength="64" required bind:value={keyName} />{/snippet}</Field>
    <div class="actions"><Button type="submit" variant="primary">Сохранить</Button></div>
  </form>
</Sheet>
<StepUpPrompt password={methods.password} passkey={methods.passkey} />

<style>
  section { margin-bottom: var(--s10); }
  .rows { list-style: none; margin: var(--s0) var(--s0) var(--s6); padding: var(--s0); display: grid; gap: var(--s2); }
  .row { display: grid; gap: var(--s1); width: 100%; padding: var(--s5) var(--s8); border-radius: var(--radius); background: var(--raised); text-align: start; color: var(--text); }
  .secondary-content .row { border-radius: var(--radius); padding: var(--s5) var(--s8); }
  .title { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); font-weight: var(--weight-medium); }
  .sub { color: var(--secondary); font-size: var(--fs-meta); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); align-items: center; }
  .gap { margin-top: var(--s6); }
  .banner { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); padding: var(--s5) var(--s8); border-radius: var(--radius-card); background: var(--warning-soft); }
  .banner p { flex: 1; min-width: 0; margin: var(--s0); }
  .panel { padding: var(--s8); border-radius: var(--radius-card); background: var(--surface); display: grid; gap: var(--s6); }
  .ok { color: var(--success); }
  .password, .rename { display: grid; gap: var(--s6); }
  .hidden-login { position: absolute; width: var(--hairline); height: var(--hairline); opacity: 0; pointer-events: none; }
  .details { display: grid; gap: var(--s5); margin: var(--s0) var(--s0) var(--s8); }
  .details div { display: grid; gap: var(--s1); }
  dt { color: var(--muted); font-size: var(--fs-sm); }
  dd { margin: var(--s0); }
  .ua { font-family: var(--font-mono); font-size: var(--fs-sm); overflow-wrap: anywhere; color: var(--secondary); }
  .danger { color: var(--danger); display: contents; --button-bg: var(--danger-soft); }
  .attribution { margin-top: var(--s10); }
</style>
