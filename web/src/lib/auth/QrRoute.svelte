<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { me } from '#lib/api/client.js';
  import { AuthRequired } from '#lib/api/http.js';
  import { errorText } from '#lib/api/errors.js';
  import { takeFragment } from './scan.js';
  import QrApprove from './QrApprove.svelte';

  let secret = $state('');
  let status = $state<'checking' | 'signed_in' | 'signed_out' | 'empty' | 'failed'>('checking');
  let error = $state('');

  onMount(() => {
    // Секрет живёт только в памяти этой страницы: из адреса и истории он уходит сразу.
    secret = takeFragment(() => void goto(resolve('/qr'), { replace: true, reset: false }));
    if (!secret) { status = 'empty'; return; }
    me().then(() => { status = 'signed_in'; }, (err) => {
      if (err instanceof AuthRequired) status = 'signed_out';
      else { status = 'failed'; error = errorText(err, 'Не удалось проверить вход'); }
    });
  });
</script>

<main id="main" tabindex="-1" class="auth-route" data-qr-route={status}>
  <div class="card">
    {#if status === 'signed_in'}<QrApprove {secret} />
    {:else if status === 'signed_out'}
      <h1>Код входа в canterlot</h1>
      <p>Откройте этот код на устройстве, где вы уже вошли в canterlot, или отсканируйте его в приложении: Настройки → Сеансы и вход → Сканировать код</p>
      <a href={resolve('/')}>Войти на этом устройстве</a>
    {:else if status === 'empty'}
      <h1>Код входа в canterlot</h1>
      <p>В ссылке нет кода входа. Отсканируйте код ещё раз</p>
      <a href={resolve('/')}>К сессиям</a>
    {:else if status === 'failed'}
      <h1>Код входа в canterlot</h1>
      <p class="error" role="alert">{error}</p>
    {:else}<p role="status">Проверяю вход…</p>{/if}
    {#if status === 'signed_in'}<a href={resolve('/settings/sign-in')}>К сеансам</a>{/if}
  </div>
</main>

<style>
  .auth-route { flex: 1; min-height: 0; overflow: auto; display: flex; align-items: center; justify-content: center; padding: max(var(--s10), env(safe-area-inset-top)) var(--s8) max(var(--s10), env(safe-area-inset-bottom)); }
  .card { width: 100%; max-width: calc(var(--phone-max) - var(--s12)); display: flex; flex-direction: column; gap: var(--s8); padding: var(--s10); border-radius: var(--radius-sheet); background: var(--surface); }
  h1 { margin: var(--s0); font-size: var(--fs-title); }
  p { margin: var(--s0); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
  .error { color: var(--danger); }
</style>
