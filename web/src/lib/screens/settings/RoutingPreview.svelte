<script lang="ts">
  import { previewRouting } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import { effortLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import { asDecision, skippedText, type Considered, type ModelTitles, type RouteDecision } from '#lib/routing/decision.js';
  import { detailText, reasonText } from '#lib/routing/reasons.js';
  import Button from '#lib/ui/Button.svelte';
  import Select from '#lib/ui/Select.svelte';
  import RouteDecisionView from '../chat/RouteDecisionView.svelte';
  import { LEVELS } from './routingPolicy.js';

  let { types, titles, stale = false }: { types: { key: string; title: string }[]; titles?: ModelTitles; stale?: boolean } = $props();

  type Machine = { free_disk_gb?: number | null; load_per_core?: number | null; mem_available_gb?: number | null; observed_at?: string | null; stale: boolean };
  type Guard = { blocked: boolean; stale: boolean; violations: string[]; detail?: string | null };

  let taskType = $state('');
  let level = $state('');
  let workdir = $state('');
  let busy = $state(false);
  let error = $state('');
  let decision = $state<RouteDecision | null>(null);
  let machine = $state<Machine | null>(null);
  let guard = $state<Guard | null>(null);
  let refused = $state<{ candidates: Considered[]; reset: number | null } | null>(null);

  $effect(() => {
    if (!types.some((item) => item.key === taskType)) taskType = types[0]?.key ?? '';
  });

  const typeOptions = $derived(types.map((item) => ({ value: item.key, label: `${item.title} (${item.key})` })));
  const levelOptions = [{ value: '', label: 'Не задан' }, ...LEVELS.map((value) => ({ value, label: effortLabel(value) }))];

  function gb(value: number | null | undefined): string | null {
    return typeof value === 'number' ? `${value.toFixed(value < 10 ? 1 : 0).replace('.', ',')} ГБ` : null;
  }

  const machineText = $derived.by(() => {
    if (!machine) return '';
    const parts = [
      gb(machine.free_disk_gb) && `диск свободно ${gb(machine.free_disk_gb)}`,
      typeof machine.load_per_core === 'number' && `нагрузка ${machine.load_per_core.toFixed(2).replace('.', ',')} на ядро`,
      gb(machine.mem_available_gb) && `память ${gb(machine.mem_available_gb)}`
    ].filter(Boolean);
    const at = machine.observed_at ? Date.parse(machine.observed_at) : Number.NaN;
    const when = Number.isFinite(at) ? ` · замер ${fmt.clock(at)}` : '';
    return parts.length > 0 ? `${parts.join(' · ')}${when}` : 'Метрик машины нет';
  });

  const guardText = $derived.by(() => {
    if (!guard) return '';
    if (guard.stale) return 'Страж не включается: метрики машины устарели, запуск пойдёт сразу';
    if (!guard.blocked) return 'Страж: машина свободна, запуск пойдёт сразу';
    const why = guard.detail ? detailText(guard.detail) : guard.violations.map((item) => reasonText(item)).join('; ');
    return `Страж: запуск встанет в очередь — ${why}`;
  });

  async function check() {
    if (!taskType || busy) return;
    busy = true;
    error = '';
    decision = null;
    machine = null;
    guard = null;
    refused = null;
    try {
      const answer = await previewRouting({ task_type: taskType, level: level || null, workdir: workdir.trim() || null });
      decision = asDecision(answer.data.decision);
      machine = answer.data.machine as Machine;
      guard = answer.data.guard as Guard;
    } catch (err) {
      if (err instanceof ApiError && err.code === 'no_executor_available') {
        // Разбор по кандидатам и ближайший сброс хаб отдаёт полями `details`.
        const details = err.details as { candidates?: Considered[] | null; nearest_reset?: string | null } | undefined;
        const reset = details?.nearest_reset ? Date.parse(details.nearest_reset) : Number.NaN;
        refused = { candidates: details?.candidates ?? [], reset: Number.isFinite(reset) ? reset : null };
      } else error = errorText(err, 'Не удалось проверить маршрут');
    } finally {
      busy = false;
    }
  }
</script>

<section class="preview" aria-labelledby="preview-title" data-role="routing-preview">
  <h2 id="preview-title">Проверить</h2>
  <p class="note">Какого исполнителя маршрут выберет сейчас: по текущим лимитам, занятости и метрикам машины.{stale ? ' Проверка идёт по сохранённой политике, несохранённые правки не учитываются' : ''}</p>
  <div class="form">
    <Select label="Тип задачи" options={typeOptions} bind:value={taskType} disabled={types.length === 0} />
    <Select label="Уровень" options={levelOptions} bind:value={level} />
    <label class="field"><span>Рабочий каталог (необязательно)</span><input type="text" placeholder="/home/owner/project" bind:value={workdir} spellcheck="false" autocapitalize="off" /></label>
    {#if busy}<Button variant="primary" disabled reason="Проверяю…">Проверить</Button>{:else}<Button variant="primary" onclick={check}>Проверить</Button>{/if}
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if decision}
    <div class="result" data-role="preview-result">
      <RouteDecisionView {decision} {titles} />
      {#if machineText}<p class="machine" data-role="preview-machine">Машина: {machineText}</p>{/if}
      {#if guardText}<p class="guard" data-role="preview-guard" data-blocked={guard?.blocked ? '' : undefined}>{guardText}</p>{/if}
    </div>
  {/if}
  {#if refused}
    <div class="result" data-role="preview-refused">
      <p><strong>Ни один кандидат сейчас не подходит</strong></p>
      {#if refused.candidates.length > 0}
        <ol class="refused">
          {#each refused.candidates as entry, index (index)}
            <li data-verdict={entry.verdict}>{skippedText(entry, titles)}</li>
          {/each}
        </ol>
      {/if}
      {#if refused.reset !== null}<p class="note">Ближайший сброс лимита в {fmt.clock(refused.reset)}: запуск встанет в очередь, если сброс не дальше 6 часов</p>{/if}
    </div>
  {/if}
</section>

<style>
  .preview { margin-bottom: var(--s10); }
  .form { display: grid; gap: var(--s4); }
  .field { display: grid; gap: var(--s2); min-width: 0; }
  .field > span { color: var(--secondary); font-size: var(--fs-meta); }
  .form :global(.reason) { margin-block: var(--s0); }
  .result { display: grid; gap: var(--s4); margin-top: var(--s6); padding: var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  .result p { margin: var(--s0); }
  .machine { color: var(--secondary); font-size: var(--fs-sm); }
  .guard { font-size: var(--fs-sm); }
  .guard[data-blocked] { color: var(--warning); }
  .refused { display: grid; gap: var(--s2); margin: var(--s0); padding-left: var(--s8); font-size: var(--fs-meta); }
  .error { margin: var(--s4) var(--s0) var(--s0); }
  @media (min-width: 700px) { .form { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); align-items: end; } .field { grid-column: 1 / -1; } }
</style>
