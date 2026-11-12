<script lang="ts">
  import Sheet from '#lib/ui/Sheet.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import { authApi } from './api.js';
  import { stepUp } from './stepup.svelte.js';
  import { signInErrorText } from './signin.js';
  import { getCredential, webauthnAvailable, webauthnErrorText } from './webauthn.js';

  let { password: hasPassword, passkey: hasPasskey }: { password: boolean; passkey: boolean } = $props();
  let value = $state('');
  let error = $state('');
  let busy = $state(false);
  const passkeys = $derived(hasPasskey && webauthnAvailable());

  $effect(() => {
    if (stepUp.open) return;
    value = ''; error = '';
  });

  async function run(work: () => Promise<unknown>): Promise<void> {
    if (busy) return;
    busy = true; error = '';
    try { await work(); value = ''; stepUp.confirmed(); }
    catch (err) { error = err instanceof DOMException ? webauthnErrorText(err) : signInErrorText(err, 'Не удалось подтвердить вход'); }
    finally { busy = false; }
  }

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    void run(() => authApi.reauthPassword(value));
  }

  function withPasskey(): void {
    void run(async () => {
      const challenge = await authApi.reauthPasskeyStart();
      await authApi.reauthPasskeyFinish(challenge.challenge_id, await getCredential(challenge.options));
    });
  }
</script>

<Sheet open={stepUp.open} title="Подтвердите вход" routeKey="step-up" onclose={() => stepUp.cancel()}>
  <div class="body" data-step-up>
    <p>Для этого действия нужен недавний вход: введите пароль или используйте ключ доступа</p>
    {#if hasPassword}
      <form onsubmit={submit}>
        <Field label="Пароль" {error}>{#snippet children({ id, describedby, invalid })}<input {id} type="password" name="password" autocomplete="current-password" bind:value aria-describedby={describedby} aria-invalid={invalid} />{/snippet}</Field>
        <Button type="submit" variant="primary" full busy={busy}>Подтвердить</Button>
      </form>
    {:else if error}<p class="error" role="alert">{error}</p>{/if}
    {#if passkeys}<Button full onclick={withPasskey}>Подтвердить ключом доступа</Button>{/if}
    {#if !hasPassword && !passkeys}<p class="note">Пароля и ключей доступа пока нет. Войдите по ссылке восстановления, чтобы подтвердить действие</p>{/if}
  </div>
</Sheet>

<style>
  .body, form { display: flex; flex-direction: column; gap: var(--s6); }
  p { margin: var(--s0); }
  .note { color: var(--muted); font-size: var(--fs-meta); }
  .error { color: var(--danger); }
</style>
