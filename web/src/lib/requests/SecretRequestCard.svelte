<script lang="ts">
  import {
    isSecretValueTooLarge,
    secretRequestCanRevoke,
    secretRequestErrorMessage,
    secretRequestLine,
    secretRequestShowsField,
    secretRequestTitle,
    type SecretRequestAnswer,
    type SecretRequestView
  } from '#lib/requests/secretRequest.js';

  let {
    view,
    onAnswer,
    onRevoke,
    disabled = false
  }: {
    view: SecretRequestView;
    onAnswer: (body: SecretRequestAnswer) => Promise<void>;
    onRevoke: () => Promise<void>;
    disabled?: boolean;
  } = $props();

  let value = $state('');
  let inputEl: HTMLInputElement | undefined = $state();
  let busy = $state<'sending' | 'revoking' | 'declining' | null>(null);
  let error = $state<string | null>(null);

  const line = $derived(
    busy === 'sending'
      ? 'Передаю на машину…'
      : busy === 'revoking'
        ? 'Отзываю…'
        : secretRequestLine(view)
  );
  const showsInput = $derived(
    (secretRequestShowsField(view) || view.state === 'sending') && busy !== 'declining'
  );
  const canAnswer = $derived(
    secretRequestShowsField(view) && busy !== 'sending' && busy !== 'declining'
  );
  const canRevoke = $derived(secretRequestCanRevoke(view) && !busy && !disabled);
  const isDisabled = $derived(disabled || busy !== null || !secretRequestShowsField(view));

  function clearValue() {
    value = '';
    if (inputEl) inputEl.value = '';
  }

  // Значение живёт только в этом поле: уход со страницы и размонтирование его стирают.
  $effect(() => {
    const clear = () => clearValue();
    window.addEventListener('pagehide', clear);
    return () => {
      window.removeEventListener('pagehide', clear);
      clear();
    };
  });

  function disableIf(node: HTMLButtonElement, val: boolean) {
    node.disabled = val;
    return {
      update(v: boolean) {
        node.disabled = v;
      }
    };
  }

  async function send() {
    if (disabled || busy) return;
    if (isSecretValueTooLarge(value)) {
      clearValue();
      error = 'Слишком большое значение';
      return;
    }
    const body: SecretRequestAnswer = { value };
    clearValue();
    error = null;
    busy = 'sending';
    try {
      await onAnswer(body);
    } catch (err) {
      error = secretRequestErrorMessage(err);
    } finally {
      clearValue();
      busy = null;
    }
  }

  async function decline() {
    if (disabled || busy) return;
    clearValue();
    error = null;
    busy = 'declining';
    try {
      await onAnswer({ decline: true });
    } catch (err) {
      error = secretRequestErrorMessage(err);
    } finally {
      clearValue();
      busy = null;
    }
  }

  async function revoke() {
    if (disabled || busy) return;
    error = null;
    busy = 'revoking';
    try {
      await onRevoke();
    } catch (err) {
      error = secretRequestErrorMessage(err);
    } finally {
      busy = null;
    }
  }
</script>

<section class="secret-request-card" data-secret-request data-state={view.state}>
  <div class="card-head">
    <p class="title">{secretRequestTitle()}</p>
  </div>

  <div class="subject">
    <p class="name">{view.name}</p>
    {#if view.purpose}<p class="purpose">{view.purpose}</p>{/if}
  </div>

  {#if line}<p class="state" data-secret-state={view.state}>{line}</p>{/if}

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if showsInput}
    <input
      bind:this={inputEl}
      bind:value={value}
      type="password"
      class="secret-input"
      data-secret-input
      aria-label={view.name}
      autocomplete="off"
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
      data-1p-ignore=""
      data-lpignore="true"
      data-bwignore="true"
      data-form-type="other"
      disabled={isDisabled}
    />
  {/if}

  {#if canAnswer || canRevoke}
    <div class="actions">
      {#if canAnswer}
        <button type="button" class="primary" use:disableIf={isDisabled} onclick={send}>
          Передать
        </button>
        <button type="button" class="secondary" use:disableIf={isDisabled} onclick={decline}>
          Отказать
        </button>
      {:else}
        <button type="button" class="secondary" use:disableIf={!canRevoke} onclick={revoke}>
          Отозвать
        </button>
      {/if}
    </div>
  {/if}
</section>

<style>
  .secret-request-card {
    flex: 0 1 auto;
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    min-width: 0;
    min-height: 0;
    overflow-x: clip;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
    box-sizing: border-box;
    width: 100%;
    color: var(--text);
  }

  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .title {
    margin: var(--s0);
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  .subject {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    min-width: 0;
  }

  .name {
    margin: var(--s0);
    min-width: 0;
    font-size: var(--fs-title);
    font-weight: var(--weight-semibold);
    line-height: var(--lh-tight);
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .purpose {
    margin: var(--s0);
    color: var(--secondary);
    font-size: var(--fs-body);
    overflow-wrap: anywhere;
  }

  .state {
    margin: var(--s0);
    color: var(--text);
    font-size: var(--fs-body);
    overflow-wrap: anywhere;
  }

  .error {
    padding: var(--s2) var(--s3);
    border-radius: var(--radius-sm);
    background: var(--danger-soft);
    color: var(--text);
    font-size: var(--fs-meta);
    overflow-wrap: anywhere;
  }

  .secret-input {
    width: 100%;
    box-sizing: border-box;
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
    font-size: var(--fs-body);
    font-family: var(--font-sans);
  }

  .secret-input::placeholder {
    color: var(--muted);
    opacity: 1;
  }

  .secret-input:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .secret-input:disabled {
    color: var(--muted);
    cursor: not-allowed;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
    margin-top: var(--s2);
  }

  .primary,
  .secondary {
    flex: 1;
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    cursor: pointer;
  }

  .primary {
    background: var(--accent);
    color: var(--accent-ink);
  }

  .primary:disabled {
    background: var(--accent-soft);
    color: var(--muted);
    cursor: not-allowed;
  }

  .secondary {
    background: var(--raised);
    color: var(--text);
  }

  .secondary:disabled {
    color: var(--muted);
    cursor: not-allowed;
  }

  .primary:focus-visible,
  .secondary:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .primary:not(:disabled):hover,
  .secondary:not(:disabled):hover {
    filter: brightness(1.1);
  }
</style>
