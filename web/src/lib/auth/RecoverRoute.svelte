<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { ApiError } from '#lib/api/http.js';
  import { authApi } from './api.js';
  import { deviceHints } from './device.js';
  import { takeFragment } from './scan.js';
  import { afterSignIn, signInErrorText } from './signin.js';

  let status = $state<'working' | 'failed'>('working');
  let error = $state('');

  onMount(() => {
    // Токен одноразовый: из адреса он уходит до запроса и нигде не сохраняется.
    const token = takeFragment(() => void goto(resolve('/recover'), { replace: true, reset: false }));
    if (!token) { status = 'failed'; error = 'В ссылке нет токена восстановления'; return; }
    void (async () => {
      try {
        await authApi.recovery(token, await deviceHints());
        afterSignIn('/settings/sign-in?from=recovery');
      } catch (err) {
        status = 'failed';
        error = err instanceof ApiError && err.code === 'invalid_credentials'
          ? 'Ссылка восстановления недействительна: она уже использована, устарела или заменена новой. Создайте новую на хабе: canterlot-hub auth recovery-link'
          : signInErrorText(err, 'Не удалось войти по ссылке восстановления');
      }
    })();
  });
</script>

<main id="main" tabindex="-1" class="auth-route" data-recover-route={status}>
  <div class="card">
    <h1>Вход по ссылке восстановления</h1>
    {#if status === 'working'}<p role="status">Вхожу…</p>
    {:else}<p class="error" role="alert">{error}</p><a href={resolve('/')}>К странице входа</a>{/if}
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
