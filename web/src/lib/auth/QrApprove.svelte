<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '#lib/ui/Button.svelte';
  import { ApiError } from '#lib/api/http.js';
  import { authApi, type QrRequest } from './api.js';
  import { placeText } from './sessions.js';
  import { signInErrorText } from './signin.js';

  let { secret, ondone }: { secret: string; ondone?: (state: 'approved' | 'denied' | 'failed') => void } = $props();

  let request = $state<QrRequest | null>(null);
  let error = $state('');
  let decided = $state<'' | 'approved' | 'denied'>('');
  let busy = $state(false);

  onMount(() => {
    authApi.qrInspect(secret).then((value) => { request = value; }, (err) => { error = qrErrorText(err); ondone?.('failed'); });
  });

  function qrErrorText(err: unknown): string {
    return err instanceof ApiError && (err.code === 'qr_expired' || err.status === 410)
      ? 'Код устарел или уже использован. Попросите показать новый код'
      : signInErrorText(err, 'Не удалось проверить код');
  }

  async function decide(approve: boolean): Promise<void> {
    if (busy) return;
    busy = true; error = '';
    try {
      await (approve ? authApi.qrApprove(secret) : authApi.qrDeny(secret));
      decided = approve ? 'approved' : 'denied';
      ondone?.(decided);
    } catch (err) { error = qrErrorText(err); }
    finally { busy = false; }
  }
</script>

<section class="approve" aria-labelledby="approve-title" data-qr-approve>
  <h2 id="approve-title">Запрос на вход</h2>
  {#if decided === 'approved'}<p role="status" class="done">Вход разрешён. Новое устройство войдёт через несколько секунд</p>
  {:else if decided === 'denied'}<p role="status">Вход отклонён</p>
  {:else if request}
    <dl>
      <div><dt>IP-адрес</dt><dd class="prominent">{request.ip ?? 'IP неизвестен'}</dd></div>
      <div><dt>Место</dt><dd class="prominent">{placeText(request.location) ?? 'неизвестно'}</dd></div>
      <div><dt>Код</dt><dd class="prominent">код показан {request.age_seconds} с назад</dd></div>
      <div><dt>Устройство</dt><dd>заявлено устройством: {request.label ?? 'неизвестно'}</dd></div>
    </dl>
    {#if request.other_country}<p class="warning" role="alert">Запрос пришёл из другой страны, чем этот сеанс. Если вы не входите сейчас сами из другой страны, отклоните его</p>{/if}
    <p class="text">Запрашивает вход в canterlot. Разрешайте, только если этот код сейчас показан на вашем собственном экране.</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="actions">
      <Button variant="primary" busy={busy} onclick={() => void decide(true)}>Разрешить</Button>
      <Button busy={busy} onclick={() => void decide(false)}>Отклонить</Button>
    </div>
  {:else if error}<p class="error" role="alert">{error}</p>
  {:else}<p role="status">Проверяю код…</p>{/if}
</section>

<style>
  .approve { display: flex; flex-direction: column; gap: var(--s6); }
  h2 { margin: var(--s0); font-size: var(--fs-title); }
  p { margin: var(--s0); }
  dl { display: grid; gap: var(--s4); margin: var(--s0); }
  dl div { display: grid; gap: var(--s1); }
  dt { color: var(--muted); font-size: var(--fs-sm); }
  dd { margin: var(--s0); color: var(--secondary); }
  .prominent { color: var(--text); font-size: var(--fs-title); font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; }
  .warning { padding: var(--s5) var(--s8); border-radius: var(--radius); background: var(--warning-soft); color: var(--text); }
  .error { color: var(--danger); }
  .done { color: var(--success); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); }
</style>
