<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { browserRuntime, openChatSession, sessionUploads } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { takeShare, type ShareItem } from '#lib/pwa/share.js';
  import { leaveAttachmentNotice } from '#lib/composer/attachmentNotice.js';
  import { ComposerModel } from '#lib/composer/model.svelte.js';
  import { loadConfig } from '#lib/api/config.js';
  import { canSendNow } from '#lib/api/gate.js';
  import { fmt } from '#lib/format/ru.js';
  import Button from '#lib/ui/Button.svelte';
  import AttachmentTray from '#lib/composer/AttachmentTray.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import { applyShare } from './model.js';
  import '../secondary.css';

  let runtime = $state<Runtime | null>(null); let item = $state<ShareItem | null>(null); let loading = $state(true); let busy = $state(false); let query = $state(''); let error = $state(''); let target = $state('');
  const id = $derived(page.url.searchParams.get('id') ?? '');
  const inactive = $derived(page.url.searchParams.get('error') === 'sw_inactive');
  const sessions = $derived(runtime ? [...runtime.list.items.values()].filter(s => !s.archived && `${s.title} ${s.machine.name}`.toLocaleLowerCase('ru-RU').includes(query.toLocaleLowerCase('ru-RU'))).sort((a, b) => Date.parse(b.last_activity_at ?? b.created_at) - Date.parse(a.last_activity_at ?? a.created_at)) : []);
  onMount(() => {
    let alive = true;
    if (inactive) { loading = false; return; }
    void browserRuntime().then(async rt => {
      const record = id ? await rt.db.get<ShareItem>('share_inbox', id) : null;
      if (!alive) return;
      runtime = rt;
      item = record && Date.now() - record.receivedAt < 86_400_000 ? record : null;
      loading = false;
      const created = page.url.searchParams.get('conversation');
      if (created && item) await choose(created);
    }).catch(() => { if (alive) { loading = false; error = 'Не удалось открыть переданные данные. Повторите при восстановлении связи'; } });
    return () => { alive = false; };
  });
  async function choose(conversationId: string) {
    if (!item || !runtime || busy) return;
    busy = true; error = ''; target = conversationId;
    try {
      const config = await loadConfig(); const chat = await openChatSession(conversationId);
      const model = new ComposerModel({ conversationId, chat, ops: runtime.journal, drafts: runtime.drafts, uploads: sessionUploads(), config: () => config.limits, canSend: canSendNow, connection: () => runtime!.connection.status });
      model.text = (await runtime.drafts.load(conversationId))?.text ?? '';
      await applyShare(item, model, async text => { runtime!.drafts.set(conversationId, { text }); await runtime!.drafts.flush(); }, async () => { await takeShare(runtime!.db, id); });
      if (model.errorMessage) leaveAttachmentNotice(conversationId, model.errorMessage);
      await goto(resolve('/c/[id]', { id: conversationId }));
    } catch { error = 'Не удалось сохранить данные в сессии. Повторите при восстановлении связи'; }
    finally { busy = false; }
  }
</script>
<main id="main" tabindex="-1" data-secondary-ready={!loading ? '' : undefined}>
  <SecondaryHeader title="Отправить в canterlot" />
  <div class="secondary-body"><div class="secondary-content">
    {#if inactive}<p>Приложение ещё не готово принять файл: откройте canterlot и поделитесь ещё раз</p><a href={resolve('/sessions')}>Открыть canterlot</a>
    {:else if loading}<p>Открываю переданные данные…</p>
    {:else}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      {#if item}
        <div class="files">{#each item.files as file, index (index)}<p><strong>{file.name}</strong><small>{fmt.bytes(file.size)}</small></p>{/each}</div>
        {#if item.title}<p>{item.title}</p>{/if}{#if item.text}<p class="shared-text">{item.text}</p>{/if}{#if item.url}<p class="shared-text">{item.url}</p>{/if}
        <label for="share-search">Выберите сессию</label><input id="share-search" type="search" placeholder="Поиск по названию или машине" bind:value={query} />
        <h2>Недавние</h2>
        {#each sessions as session (session.id)}<div class="choice"><Button full {...(busy || Boolean(target && error) ? { disabled: true as const, reason: busy ? 'Сохраняю в сессию' : 'Откройте черновик для проверки вложений' } : { disabled: false as const })} onclick={() => choose(session.id)}><span class="choice-content"><strong>{session.title}</strong><small>{session.harness.title} · {session.machine.name}</small></span></Button></div>{:else}<p>Сессии не найдены. Измените поиск или создайте новую сессию</p>{/each}
        <a class="new" href={resolve(`/new?share=${encodeURIComponent(id)}` as '/new')}>Новая сессия</a>
        {#if busy}<p role="status">Сохраняю в сессию…</p>{/if}
        {#if target && runtime}<AttachmentTray uploads={sessionUploads()} conversationId={target} />{#if error}<a href={resolve('/c/[id]', { id: target })}>Открыть черновик сессии</a>{/if}{/if}
        <p id="share-reason" class="note">Файлы начнут загружаться после выбора сессии. Сообщение останется в черновике</p>
      {:else if !error}<p>Переданные данные не найдены или срок хранения истёк</p><a class="new" href={resolve('/sessions')}>К сессиям</a>{/if}
    {/if}
  </div></div>
</main>
<style>
  .files { padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  small { display: block; color: var(--muted); }
  input { display: block; width: 100%; min-height: var(--target); margin-block: var(--s4); padding: var(--s6); border-radius: var(--radius-pill); border: var(--s0); background: var(--surface); color: var(--text); }
  .choice-content { width: 100%; text-align: left; }
  .choice { display: block; width: 100%; margin-block: var(--s4); text-align: left; }
  .new { display: block; min-height: var(--target); padding: var(--s6); }
  .shared-text { white-space: pre-wrap; }
</style>
