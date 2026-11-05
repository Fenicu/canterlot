<script lang="ts">
  import { onMount } from 'svelte';
  import { getLimitSettings, putLimitSettings } from '#lib/api/client.js';
  import { refreshConfig } from '#lib/api/config.js';
  import { ApiError } from '#lib/api/http.js';
  import Button from '#lib/ui/Button.svelte';
  import {
    checkDraft, draftFrom, sameSettings,
    type LimitSettingsDraft, type LimitSettingsErrors, type LimitSettingsView
  } from './limitSettings.js';

  let view = $state<LimitSettingsView | null>(null);
  let draft = $state<LimitSettingsDraft>({ warn: null, danger: null, reserve: null });
  let errors = $state<LimitSettingsErrors>({});
  let error = $state('');
  let busy = $state(false);

  const dirty = $derived(view !== null && !sameSettings(draft, view.settings));
  const canReset = $derived(view !== null && (!view.is_default || dirty));

  function accept(next: LimitSettingsView) {
    view = next;
    draft = draftFrom(next.settings);
    errors = {};
  }

  async function load() {
    error = '';
    try { accept((await getLimitSettings()).data); }
    catch { error = 'Не удалось загрузить пороги лимитов. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  function inputNumber(value: string): number | null {
    if (value.trim() === '') return null;
    const number = Number(value);
    return Number.isFinite(number) ? number : null;
  }

  async function send(settings: LimitSettingsView['settings'] | null) {
    if (busy) return;
    busy = true; error = '';
    try {
      accept((await putLimitSettings({ settings }, crypto.randomUUID())).data);
      // Другие экраны читают тот же кэш конфига: новые пороги видны без перезагрузки.
      await refreshConfig().catch(() => undefined);
    } catch (err) {
      error = err instanceof ApiError ? err.message : 'Не удалось сохранить пороги. Повторите при восстановлении связи';
    } finally { busy = false; }
  }

  function save() {
    error = '';
    const checked = checkDraft(draft);
    if (!checked.ok) { errors = checked.errors; return; }
    errors = {};
    void send(checked.settings);
  }

  function reset() {
    errors = {};
    if (view?.is_default) draft = draftFrom(view.settings);
    else void send(null);
  }
</script>

<section data-limit-settings>
  <h2>Предупреждения о лимитах</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if !view}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if view}
    <div class="card">
      <label class="field">
        <span>Жёлтым, когда осталось меньше, %</span>
        <input type="number" min="0" max="99" step="1" inputmode="numeric"
          value={draft.warn ?? ''} aria-invalid={errors.warn ? 'true' : undefined}
          oninput={(event) => draft.warn = inputNumber(event.currentTarget.value)} />
      </label>
      {#if errors.warn}<p class="error" role="alert">{errors.warn}</p>{/if}
      <label class="field">
        <span>Красным, когда осталось меньше, %</span>
        <input type="number" min="0" max="99" step="1" inputmode="numeric"
          value={draft.danger ?? ''} aria-invalid={errors.danger ? 'true' : undefined}
          oninput={(event) => draft.danger = inputNumber(event.currentTarget.value)} />
      </label>
      {#if errors.danger}<p class="error" role="alert">{errors.danger}</p>{/if}
      <label class="field">
        <span>Резерв владельца: оставить себе, % недели</span>
        <input type="number" min="0" max="99" step="1" inputmode="numeric"
          value={draft.reserve ?? ''} aria-invalid={errors.reserve ? 'true' : undefined}
          oninput={(event) => draft.reserve = inputNumber(event.currentTarget.value)} />
      </label>
      {#if errors.reserve}<p class="error" role="alert">{errors.reserve}</p>{/if}
      {#if errors.relation}<p class="error" role="alert">{errors.relation}</p>{/if}
      <p class="note">Цвета — для остатка в любом окне любого провайдера. Резерв — недельное окно провайдера, который его отдаёт: когда остаток достигает порога, новые ходы встают на паузу, а «кранч» ждёт сброса недели</p>
      <div class="actions">
        {#if dirty && !busy}<Button variant="primary" onclick={save}>Сохранить</Button>{:else}<Button variant="primary" disabled reason={busy ? 'Сохраняю…' : 'Нет изменений'}>Сохранить</Button>{/if}
        {#if canReset && !busy}<Button onclick={reset}>Сбросить к значениям по умолчанию</Button>{:else}<Button disabled reason={busy ? 'Сохраняю…' : 'Уже стоят значения по умолчанию'}>Сбросить к значениям по умолчанию</Button>{/if}
      </div>
    </div>
  {:else if !error}
    <p>Загружаю пороги лимитов…</p>
  {/if}
</section>

<style>
  section { margin-bottom: var(--s10); }
  .card { display: grid; gap: var(--s4); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  .field { display: grid; gap: var(--s2); }
  .field span { color: var(--secondary); font-size: var(--fs-meta); }
  .field input { min-height: var(--target); padding: var(--s5) var(--s8); background: var(--raised); color: inherit; border: 0; border-radius: var(--radius); font-size: var(--fs-input); }
  .field input[aria-invalid='true'] { outline: var(--focus-width) solid var(--danger); }
  .actions { display: grid; gap: var(--s4); }
  .actions :global(.reason) { margin-block: var(--s0); }
</style>
