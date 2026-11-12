<script lang="ts">
  import { onMount, type Component } from 'svelte';
  import type { SignOut } from '#lib/api/http.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import { authApi, type MethodsView } from './api.js';
  import { deviceHints } from './device.js';
  import { getCredential, webauthnAvailable, webauthnErrorText } from './webauthn.js';
  import { afterSignIn, causeText, signInErrorText } from './signin.js';

  let { cause = null, wiped = false }: { cause?: SignOut | null; wiped?: boolean } = $props();

  let login = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);
  let methods = $state<MethodsView | null>(null);
  let qr = $state(false);
  let QrPanel = $state<Component<{ onsignedin: () => void; oncancel: () => void }> | null>(null);
  const passkeys = $derived(methods?.passkey === true && webauthnAvailable());
  const subtitle = $derived(causeText(cause, wiped));

  onMount(() => {
    authApi.methods().then((value) => { methods = value; }, () => undefined);
  });

  async function run(work: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true; error = '';
    try { await work(); }
    catch (err) { error = err instanceof DOMException ? webauthnErrorText(err) : signInErrorText(err); }
    finally { busy = false; }
  }

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    void run(async () => {
      await authApi.password(login, password, await deviceHints());
      afterSignIn();
    });
  }

  function withPasskey(): void {
    void run(async () => {
      const challenge = await authApi.passkeyStart();
      const credential = await getCredential(challenge.options);
      await authApi.passkeyFinish(challenge.challenge_id, credential, await deviceHints());
      afterSignIn();
    });
  }

  function openQr(): void {
    error = '';
    qr = true;
    if (!QrPanel) void import('./QrPanel.svelte').then((module) => { QrPanel = module.default; }, () => { qr = false; error = 'Не удалось открыть вход по QR. Обновите страницу'; });
  }
</script>

<main id="main" tabindex="-1" class="signin" data-signin>
  <div class="card">
    <h1>Вход в canterlot</h1>
    {#if subtitle}<p class="cause" role="status">{subtitle}</p>{/if}
    {#if qr}
      {#if QrPanel}<QrPanel onsignedin={() => afterSignIn()} oncancel={() => { qr = false; }} />{:else}<p role="status">Открываю…</p>{/if}
    {:else}
      <form onsubmit={submit} novalidate>
        <Field label="Логин">{#snippet children({ id })}<input {id} name="username" autocomplete="username" autocapitalize="none" spellcheck="false" required bind:value={login} />{/snippet}</Field>
        <Field label="Пароль">{#snippet children({ id })}<input {id} name="password" type="password" autocomplete="current-password" required bind:value={password} />{/snippet}</Field>
        {#if error}<p class="error" role="alert">{error}</p>{/if}
        <Button type="submit" variant="primary" full busy={busy}>Войти</Button>
      </form>
      <div class="alt">
        {#if passkeys}<Button full onclick={withPasskey}>Войти с ключом доступа</Button>{/if}
        <Button full onclick={openQr}>Войти по QR</Button>
      </div>
    {/if}
  </div>
</main>

<style>
  .signin { flex: 1; min-height: 0; overflow: auto; display: flex; align-items: center; justify-content: center; padding: max(var(--s10), env(safe-area-inset-top)) var(--gutter, var(--s8)) max(var(--s10), env(safe-area-inset-bottom)); background: var(--bg); }
  .card { width: 100%; max-width: calc(var(--phone-max) - var(--s12)); display: flex; flex-direction: column; gap: var(--s8); padding: var(--s10); border-radius: var(--radius-sheet); background: var(--surface); }
  h1 { margin: var(--s0); font-size: var(--fs-heading); letter-spacing: var(--tracking-tight); }
  .cause { margin: var(--s0); color: var(--secondary); }
  form, .alt { display: flex; flex-direction: column; gap: var(--s6); }
  .error { margin: var(--s0); color: var(--danger); font-size: var(--fs-meta); }
</style>
