<script lang="ts">
  import { onMount } from 'svelte';
  import { clearCloudBlock, getCloudBudget, putCloudBudget } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import { kopecksInput, parseRubles, rubles } from '#lib/format/money.js';
  import { fmt } from '#lib/format/ru.js';
  import { blockText, profileLines, type CloudBudget } from '#lib/screens/machines/cloudModel.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';

  let view = $state<CloudBudget | null>(null);
  let error = $state('');
  let fieldError = $state('');
  let notice = $state<{ text: string; tone: 'ok' | 'warning' } | null>(null);
  let budget = $state('');
  let auto = $state(false);
  let busy = $state(false);
  let confirmingBlock = $state(false);
  let clearing = $state(false);

  const parsed = $derived(parseRubles(budget));
  const dirty = $derived(
    view !== null &&
      (parsed !== view.monthly_budget_kopecks || auto !== view.auto_mode)
  );
  const block = $derived(view ? blockText(view.provider_block_code) : null);
  const blockNote = $derived.by(() => {
    if (!view?.provider_block_code) return '';
    const at = view.provider_block_at
      ? ` · ${fmt.stamp(Date.parse(view.provider_block_at), Date.now())}`
      : '';
    return `Код ${view.provider_block_code}${at}. Пока блок стоит, новые окна не создаются`;
  });
  const over = $derived(view !== null && view.remaining_kopecks < 0 ? -view.remaining_kopecks : 0);

  function accept(next: CloudBudget) {
    view = next;
    budget = kopecksInput(next.monthly_budget_kopecks);
    auto = next.auto_mode;
    fieldError = '';
  }

  async function load() {
    error = '';
    try {
      accept((await getCloudBudget()).data);
    } catch {
      error = 'Не удалось загрузить бюджет облака. Повторите при восстановлении связи';
    }
  }
  onMount(() => { void load(); });

  async function save() {
    if (busy || !view) return;
    if (parsed === null) {
      fieldError = 'Введите сумму в рублях, например 5000 или 5 000,50';
      return;
    }
    fieldError = '';
    notice = null;
    busy = true;
    try {
      const { data } = await putCloudBudget(
        {
          monthly_budget_kopecks: parsed,
          auto_mode: auto,
          base_revision: view.revision
        },
        crypto.randomUUID()
      );
      accept(data);
      notice = { text: 'Сохранено', tone: 'ok' };
    } catch (err) {
      // Устаревшая ревизия: значения уже изменились на другом устройстве. Показываем новые,
      // а не затираем их вслепую.
      if (err instanceof ApiError && err.code === 'stale_revision') {
        await load();
        notice = {
          text: 'Настройки изменились на другом устройстве. Показаны новые значения',
          tone: 'warning'
        };
      } else {
        error = errorText(err, 'Не удалось сохранить бюджет. Повторите при восстановлении связи');
      }
    } finally {
      busy = false;
    }
  }

  async function clear() {
    if (clearing) return;
    clearing = true;
    notice = null;
    error = '';
    try {
      await clearCloudBlock(crypto.randomUUID());
      confirmingBlock = false;
      await load();
      notice = { text: 'Блок снят: новые окна можно запускать', tone: 'ok' };
    } catch (err) {
      confirmingBlock = false;
      error = errorText(err, 'Не удалось снять блок. Повторите при восстановлении связи');
    } finally {
      clearing = false;
    }
  }
</script>

<section data-cloud-settings>
  <h2>Бюджет и автоматический режим</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if !view}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if view}
    {#if !view.configured}
      <p class="note" data-cloud-settings-off>Облако не настроено на хабе: бюджет и окна недоступны</p>
    {:else}
      <div class="card">
        <label class="field">
          <span class="label">Бюджет на месяц, ₽</span>
          <input
            type="text"
            inputmode="decimal"
            value={budget}
            aria-invalid={fieldError ? 'true' : undefined}
            oninput={(event) => { budget = event.currentTarget.value; fieldError = ''; }}
          />
        </label>
        {#if fieldError}<p class="error" role="alert">{fieldError}</p>{/if}
        <dl class="facts">
          <dt>Израсходовано в этом месяце</dt><dd>{rubles(view.spent_kopecks)}</dd>
          <dt>Осталось</dt><dd data-tone={over > 0 ? 'warning' : undefined}>{rubles(view.remaining_kopecks)}</dd>
        </dl>
        {#if over > 0}
          <p class="warning" data-cloud-over-budget>Расход больше бюджета на {rubles(over)}</p>
        {/if}
        <p class="note">Расход — законченные окна по итоговой цене и живые по резерву. Ноль выключает автоматический режим: окна запускаются только по карточкам</p>
        <label class="switch">
          <input type="checkbox" role="switch" checked={auto} onchange={(event) => { auto = event.currentTarget.checked; }} />
          <span class="track"></span>
          <span>Запускать окна автоматически</span>
        </label>
        <p class="note">Автоматический режим запускает окно, когда статичные раннеры недоступны десять минут, облачная работа ждёт не меньше часа, а резерв помещается в бюджет</p>
        <div class="actions">
          {#if busy}
            <Button variant="primary" disabled reason="Сохраняю…">Сохранить</Button>
          {:else if dirty}
            <Button variant="primary" onclick={save}>Сохранить</Button>
          {:else}
            <Button variant="primary" disabled reason="Нет изменений">Сохранить</Button>
          {/if}
        </div>
      </div>

      {#if block}
        <div class="card block" data-cloud-block>
          <h3>Провайдер остановил окна</h3>
          <p>{block}</p>
          <p class="note">{blockNote}</p>
          <div class="actions">
            <Button onclick={() => confirmingBlock = true}>Снять блок</Button>
          </div>
        </div>
      {/if}

      <h3 class="group-title">Профили</h3>
      <div class="card">
        {#if view.profiles.length === 0}
          <p class="note">Хаб не получил цены профилей у провайдера</p>
        {:else}
          <ul class="profiles">
            {#each profileLines(view.profiles) as profile (profile.key)}
              <li data-cloud-profile>{profile.text}</li>
            {/each}
          </ul>
          <p class="note">Цены приходят от Yandex Cloud и только для чтения: профиль и порядок заданы конфигурацией хаба</p>
        {/if}
      </div>

      {#if notice}
        <p class="notice" data-tone={notice.tone} role="status">{notice.text}</p>
      {/if}
    {/if}
  {:else if !error}
    <p>Загружаю бюджет облака…</p>
  {/if}
</section>

{#if confirmingBlock}
  <ConfirmDialog
    title="Снять блок провайдера?"
    body="Новые окна снова станут возможны. Снимайте блок, когда причина отказа устранена: иначе Yandex Cloud откажет снова и поставит блок обратно."
    confirmLabel="Снять блок"
    onconfirm={clear}
    onclose={() => confirmingBlock = false}
  />
{/if}

<style>
  section { margin-bottom: var(--s10); }
  .card {
    display: grid;
    gap: var(--s4);
    margin-bottom: var(--s6);
    padding: var(--s8);
    border-radius: var(--radius-card);
    background: var(--surface);
  }
  .card h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .field { display: grid; gap: var(--s2); }
  .label { color: var(--secondary); font-size: var(--fs-meta); }
  .field input {
    width: 100%;
    box-sizing: border-box;
    min-height: var(--target);
    padding: var(--s5) var(--s8);
    border: 0;
    border-radius: var(--radius);
    background: var(--raised);
    color: inherit;
    font-size: var(--fs-input);
  }
  .field input[aria-invalid='true'] { outline: var(--focus-width) solid var(--danger); }
  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: var(--s2) var(--s8);
    margin: var(--s0);
  }
  .facts dt { color: var(--muted); font-size: var(--fs-meta); }
  .facts dd { margin: var(--s0); font-variant-numeric: tabular-nums; }
  .facts dd[data-tone='warning'] { color: var(--warning); }
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
  .warning { margin: var(--s0); color: var(--warning); font-size: var(--fs-meta); }
  .error { color: var(--danger); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); }
  .group-title { margin: var(--s8) var(--s0) var(--s4); color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .switch {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s6);
    min-height: var(--target);
    cursor: pointer;
  }
  .switch input {
    position: absolute;
    opacity: 0;
    width: var(--toggle-width);
    height: var(--toggle-height);
    margin: 0;
    cursor: pointer;
  }
  .track {
    flex: none;
    width: var(--toggle-width);
    height: var(--toggle-height);
    border-radius: var(--radius-pill);
    background: var(--control-line);
    padding: var(--s2);
    pointer-events: none;
    white-space: nowrap;
  }
  .track::after {
    content: '';
    display: block;
    width: var(--toggle-knob);
    height: var(--toggle-knob);
    border-radius: var(--radius-round);
    background: var(--surface);
  }
  .switch input:checked + .track { background: var(--accent); }
  .switch input:checked + .track::after { margin-left: auto; background: var(--accent-ink); }
  .switch input:focus-visible + .track { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  .profiles { list-style: none; margin: var(--s0); padding: var(--s0); display: flex; flex-direction: column; gap: var(--s2); }
  .profiles li { padding-block: var(--s2); border-top: var(--hairline) solid var(--line); font-size: var(--fs-meta); }
  .profiles li:first-child { border-top: 0; }
  .notice { margin: var(--s2) var(--s0); font-size: var(--fs-meta); color: var(--secondary); }
  .notice[data-tone='warning'] { color: var(--warning); }
</style>
