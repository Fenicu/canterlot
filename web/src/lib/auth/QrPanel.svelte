<script lang="ts">
  import { onMount } from 'svelte';
  import { clock } from '#lib/clock.js';
  import { ApiError } from '#lib/api/http.js';
  import Button from '#lib/ui/Button.svelte';
  import { authApi, type QrState } from './api.js';
  import { deviceHints } from './device.js';
  import { signInErrorText } from './signin.js';
  import QrCode from './QrCode.svelte';

  let { onsignedin, oncancel }: { onsignedin: () => void; oncancel: () => void } = $props();

  let url = $state('');
  let expiresAt = $state(0);
  let shown = $state<QrState>('pending');
  let error = $state('');
  let now = $state(clock.serverNow());
  let alive = true;
  let controller: AbortController | null = null;
  let generation = 0;

  const left = $derived(Math.max(0, Math.ceil((expiresAt - now) / 1000)));

  async function start(): Promise<void> {
    controller?.abort();
    const mine = ++generation;
    error = '';
    shown = 'pending';
    try {
      const code = await authApi.qrStart(await deviceHints());
      if (!alive || mine !== generation) return;
      url = code.url;
      expiresAt = Date.parse(code.expires_at);
      void poll(code.poll_token, mine);
    } catch (err) {
      if (!alive || mine !== generation) return;
      url = '';
      error = err instanceof ApiError && err.status === 429 ? 'Слишком много запросов кода. Повторите через минуту' : signInErrorText(err, 'Не удалось получить код входа');
    }
  }

  async function poll(token: string, mine: number): Promise<void> {
    while (alive && mine === generation) {
      controller = new AbortController();
      try {
        const answer = await authApi.qrPoll(token, shown, controller.signal);
        if (!alive || mine !== generation) return;
        if (answer.state === 'approved') { shown = 'approved'; onsignedin(); return; }
        // Истёкший код (или выданный другому опросу) заменяется новым сам.
        if (answer.state === 'expired') { void start(); return; }
        shown = answer.state;
        if (answer.state === 'denied') return;
      } catch (err) {
        if (!alive || mine !== generation || controller.signal.aborted) return;
        error = signInErrorText(err, 'Нет связи с хабом');
        return;
      }
    }
  }

  onMount(() => {
    void start();
    const timer = setInterval(() => {
      now = clock.serverNow();
      // Каждые 60 с новый код, пока его никто не отсканировал.
      if (url && shown === 'pending' && !error && now >= expiresAt) void start();
    }, 1000);
    return () => { alive = false; generation++; controller?.abort(); clearInterval(timer); };
  });
</script>

<section class="qr" aria-labelledby="qr-title" data-qr-state={shown}>
  <h2 id="qr-title">Вход по QR</h2>
  <p class="note">Отсканируйте код на устройстве, где вы уже вошли: Настройки → Сеансы и вход → Сканировать код</p>
  {#if url && shown !== 'denied'}
    <div class="frame" class:dim={shown === 'scanned'}><QrCode value={url} label="Код входа в canterlot" /></div>
  {/if}
  <p role="status" class="state" data-tone={shown === 'denied' || error ? 'danger' : shown === 'scanned' ? 'accent' : undefined}>
    {#if error}{error}
    {:else if shown === 'scanned'}Код отсканирован, подтвердите на другом устройстве
    {:else if shown === 'denied'}Вход отклонён на другом устройстве
    {:else if shown === 'approved'}Вход подтверждён, открываю…
    {:else if url}Код обновится через {left} с
    {:else}Получаю код…{/if}
  </p>
  <div class="actions">
    {#if error || shown === 'denied'}<Button variant="primary" onclick={() => void start()}>Показать новый код</Button>{/if}
    <Button variant="text" onclick={oncancel}>Отмена</Button>
  </div>
</section>

<style>
  .qr { display: flex; flex-direction: column; align-items: center; gap: var(--s4); text-align: center; }
  h2 { margin: var(--s0); font-size: var(--fs-title); }
  .note { margin: var(--s0); color: var(--secondary); font-size: var(--fs-meta); }
  .frame { padding: var(--s4); border-radius: var(--radius-card); background: var(--qr-light); transition: opacity var(--duration-fast) var(--ease); }
  .dim { opacity: var(--opacity-low); }
  .state { margin: var(--s0); min-height: calc(var(--fs-body) * var(--lh-body)); font-variant-numeric: tabular-nums; }
  [data-tone='danger'] { color: var(--danger); }
  [data-tone='accent'] { color: var(--accent-link); font-weight: var(--weight-medium); }
  .actions { display: flex; flex-wrap: wrap; justify-content: center; gap: var(--s4); }
</style>
