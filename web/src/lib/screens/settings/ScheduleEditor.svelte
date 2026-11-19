<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { createSchedule, getRoutingPolicy, getScheduleDefaults, getSchedule, patchSchedule, previewSchedule } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import { catalogModelLabel } from '#lib/format/modelResolution.js';
  import { DEFAULT_PERMISSION_MODE, effortLabel, permissionHint, permissionLabel } from '#lib/format/labels.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import type { SchedulesStore } from '#lib/stores/schedules.svelte.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import WorkdirField from '../sessions/WorkdirField.svelte';
  import { keptHarness, machineProblem, workdirProblem } from '../sessions/newSessionModel.js';
  import { sortPresets, type PresetSummaryLike } from '../presets/presetsModel.js';
  import {
    CRON_PRESETS, OVERLAP_OPTIONS, defaultTimezone, fireTime, parseCron, type Schedule, type ScheduleTarget
  } from './schedules.js';

  let { schedule = null, runtime, store, done, cancel }: {
    schedule: Schedule | null;
    runtime: Runtime;
    store: SchedulesStore;
    done: (scheduleId: string) => void;
    cancel: () => void;
  } = $props();

  const browserZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  // Форма живёт одной правкой: пропс читаем только один раз при открытии редактора.
  const initial = untrack(() => {
    const target = schedule?.target;
    const isNew = target?.kind === 'new_session';
    return {
      name: schedule?.name ?? '',
      cron: schedule?.cron ?? '0 9 * * *',
      timezone: schedule?.timezone ?? defaultTimezone(null, browserZone),
      prompt: schedule?.prompt ?? '',
      precheck: schedule?.precheck ?? '',
      precheckTimeout: schedule?.precheck_timeout_seconds ?? 60,
      catchUp: schedule?.catch_up_minutes ?? 30,
      overlap: schedule?.overlap ?? 'skip',
      titleTemplate: schedule?.title_template ?? '{name} · {date}',
      targetKind: (target?.kind ?? 'new_session') as 'new_session' | 'existing_session' | 'session_report',
      machineId: isNew && target?.kind === 'new_session' ? target.machine_id : '',
      path: isNew && target?.kind === 'new_session' ? target.workdir : '',
      executorMode: (isNew && target?.kind === 'new_session' && target.executor ? 'explicit' : 'route') as 'route' | 'explicit',
      taskType: isNew && target?.kind === 'new_session' ? (target.route?.task_type ?? '') : '',
      level: isNew && target?.kind === 'new_session' ? (target.route?.level ?? '') : '',
      harness: isNew && target?.kind === 'new_session' ? (target.executor?.harness ?? '') : '',
      modelId: isNew && target?.kind === 'new_session' ? (target.executor?.model ?? '') : '',
      effort: isNew && target?.kind === 'new_session' ? (target.executor?.effort ?? '') : '',
      presetId: isNew && target?.kind === 'new_session' ? (target.preset ?? 'clean') : 'clean',
      permission: isNew && target?.kind === 'new_session' ? (target.permission_mode ?? '') : '',
      groupId: isNew && target?.kind === 'new_session' ? (target.group_id ?? '') : '',
      sessionId: target?.kind === 'existing_session' || target?.kind === 'session_report' ? target.conversation_id : '',
      baseRevision: schedule?.revision ?? 0
    };
  });
  let name = $state(initial.name);
  let cron = $state(initial.cron);
  let timezone = $state(initial.timezone);
  let prompt = $state(initial.prompt);
  let precheck = $state(initial.precheck);
  let precheckTimeout = $state<number | null>(initial.precheckTimeout);
  let catchUp = $state<number | null>(initial.catchUp);
  let overlap = $state(initial.overlap);
  let titleTemplate = $state(initial.titleTemplate);
  let targetKind = $state(initial.targetKind);
  let machineId = $state(initial.machineId);
  let path = $state(initial.path);
  let executorMode = $state(initial.executorMode);
  let taskType = $state(initial.taskType);
  let level = $state(initial.level);
  let harness = $state(initial.harness);
  let modelId = $state(initial.modelId);
  let effort = $state(initial.effort);
  let presetId = $state(initial.presetId);
  let permission = $state(initial.permission);
  let groupId = $state(initial.groupId);
  let sessionId = $state(initial.sessionId);
  // Отказ хаба по цели (`details.field = target`) стоит у поля сессии и гаснет, когда цель меняется.
  let targetError = $state('');

  let preview = $state<string[]>([]);
  let previewError = $state('');
  let previewing = $state(false);
  let error = $state('');
  let conflict = $state(false);
  let saving = $state(false);
  let baseRevision = $state(initial.baseRevision);
  let taskTypes = $state<{ key: string; title: string }[]>([]);
  const opKey = crypto.randomUUID();

  const BUILTIN_PRESETS: PresetSummaryLike[] = [
    { id: 'clean', kind: 'clean', purpose: 'instructions', title: 'Без инструкций', description: 'Без личных инструкций владельца', revision: null, text_bytes: null, created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null }, last_edit: null, used_by: 0 },
    { id: 'owner', kind: 'owner', purpose: 'instructions', title: 'Как у владельца', description: 'Конфигурация агента владельца без изменений', revision: null, text_bytes: null, created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null }, last_edit: null, used_by: 0 }
  ];

  const machines = $derived([...runtime.machines.items.values()]);
  const machine = $derived(machines.find((item) => item.id === machineId));
  const harnesses = $derived(machine?.harnesses ?? []);
  const selectedHarness = $derived(harnesses.find((item) => item.id === harness));
  const models = $derived(selectedHarness?.models ?? []);
  const efforts = $derived(models.find((item) => item.id === modelId)?.efforts ?? []);
  const permissions = $derived(selectedHarness?.permission_modes ?? []);
  // Боковая копия — скрытый читатель родителя: ни отчёт, ни ввод ей не адресуют.
  const sessions = $derived([...runtime.list.items.values()].filter((item) => !item.archived && !item.side_of).sort((a, b) => Date.parse(b.last_activity_at ?? b.created_at) - Date.parse(a.last_activity_at ?? a.created_at)));
  const groups = $derived(runtime.list.groups);
  const presets = $derived(runtime.list.presets.length > 0 ? sortPresets([...runtime.list.presets]) : BUILTIN_PRESETS);

  const machineOptions = $derived([
    { value: '', label: 'Выберите машину' },
    ...machines.map((item) => {
      const problem = machineProblem(item);
      return { value: item.id, label: `${item.name}${problem ? ` · ${problem}` : ''}`, disabled: Boolean(problem) };
    })
  ]);
  const taskOptions = $derived([
    { value: '', label: 'Выберите тип задачи' },
    ...taskTypes.map((item) => ({ value: item.key, label: item.title || item.key }))
  ]);
  const levelOptions = $derived([
    { value: '', label: 'По умолчанию' },
    ...['low', 'medium', 'high', 'xhigh'].map((value) => ({ value, label: value }))
  ]);
  const modelOptions = $derived([{ value: '', label: 'Выбрать' }, ...models.map((item) => ({ value: item.id, label: catalogModelLabel(item) }))]);
  const effortOptions = $derived([{ value: '', label: 'Выбрать' }, ...efforts.map((item) => ({ value: item, label: effortLabel(item) }))]);
  const permissionOptions = $derived(permissions.map((value) => ({ value, label: permissionLabel(value) })));
  const presetOptions = $derived(presets.map((item) => ({ value: item.id, label: item.title })));
  const sessionOptions = $derived([
    { value: '', label: 'Выберите сессию' },
    ...sessions.map((item) => ({ value: item.id, label: `${item.title} · ${item.machine.name}` }))
  ]);
  const groupOptions = $derived([
    { value: '', label: 'Без группы' },
    ...groups.map((item) => ({ value: item.id, label: item.name }))
  ]);

  onMount(() => {
    void getRoutingPolicy().then((response) => {
      const view = response.data as { builtin?: { task_types?: Record<string, { title?: string }> }; policy?: { task_types?: Record<string, { title?: string }> } | null };
      const types = { ...(view.builtin?.task_types ?? {}), ...(view.policy?.task_types ?? {}) };
      taskTypes = Object.entries(types).map(([key, value]) => ({ key, title: value.title ?? key }));
    }).catch(() => undefined);
    // Новое расписание берёт пояс владельца; сохранённое остаётся при своём поясе.
    if (schedule) { void refreshPreview(); return; }
    void getScheduleDefaults()
      .then((response) => {
        if (timezone === initial.timezone) timezone = defaultTimezone(response.data.timezone, browserZone);
      })
      .catch(() => undefined)
      .finally(() => { void refreshPreview(); });
  });

  $effect(() => { void sessionId; void targetKind; targetError = ''; });
  $effect(() => { void machineId; harness = keptHarness(harnesses, harness); });
  $effect(() => { void harness; if (modelId && !models.some((item) => item.id === modelId)) modelId = models[0]?.id ?? ''; });
  $effect(() => {
    const modes = permissions;
    if (modes.length > 0 && !modes.includes(permission)) {
      permission = modes.includes(DEFAULT_PERMISSION_MODE) ? DEFAULT_PERMISSION_MODE : (modes[0] ?? '');
    }
  });

  async function refreshPreview() {
    if (!parseCron(cron)) { preview = []; previewError = 'Некорректное выражение cron'; return; }
    previewing = true; previewError = '';
    try {
      preview = (await previewSchedule({ cron: cron.trim(), timezone, count: 5 })).data;
    } catch {
      preview = [];
      previewError = 'Не удалось получить времена запусков. Проверьте выражение cron';
    } finally { previewing = false; }
  }

  function pickPreset(value: string) {
    const preset = CRON_PRESETS.find((item) => item.id === value);
    if (preset) cron = preset.cron;
  }

  function validate(): string | null {
    if (!name.trim()) return 'Укажите название';
    if (!parseCron(cron)) return 'Некорректное выражение cron';
    if (!prompt.trim()) return 'Введите подсказку для запуска';
    if (targetKind === 'new_session') {
      if (!machineId || machineProblem(machine)) return 'Выберите доступную машину';
      if (!path.trim() || workdirProblem(path.trim())) return 'Укажите корректную рабочую папку';
      if (executorMode === 'route') { if (!taskType) return 'Выберите тип задачи'; }
      else if (!harness || !modelId || !effort) return 'Выберите агента, модель и усилие';
    } else if (!sessionId) return 'Выберите сессию';
    if (precheckTimeout !== null && (!Number.isInteger(precheckTimeout) || precheckTimeout < 1 || precheckTimeout > 300)) return 'Таймаут предпроверки: от 1 до 300 секунд';
    if (catchUp !== null && (!Number.isInteger(catchUp) || catchUp < 0 || catchUp > 1440)) return 'Окно догона: от 0 до 1440 минут';
    return null;
  }

  function buildTarget(): ScheduleTarget {
    if (targetKind === 'session_report') return { kind: 'session_report', conversation_id: sessionId };
    if (targetKind === 'existing_session') return { kind: 'existing_session', conversation_id: sessionId };
    return {
      kind: 'new_session',
      machine_id: machineId,
      workdir: path.trim().replace(/(?<=.)\/+$/, ''),
      ...(executorMode === 'route'
        ? { route: { task_type: taskType, ...(level ? { level } : {}) } }
        : { executor: { harness, model: modelId, effort } }),
      ...(presetId ? { preset: presetId } : {}),
      ...(permission ? { permission_mode: permission } : {}),
      ...(groupId ? { group_id: groupId } : {})
    };
  }

  async function refresh() {
    if (!schedule) return;
    try {
      const fresh = (await getSchedule(schedule.id)).data;
      baseRevision = fresh.revision;
      name = fresh.name; cron = fresh.cron; timezone = fresh.timezone;
      prompt = fresh.prompt; precheck = fresh.precheck ?? '';
      precheckTimeout = fresh.precheck_timeout_seconds; catchUp = fresh.catch_up_minutes;
      overlap = fresh.overlap; titleTemplate = fresh.title_template;
      targetKind = fresh.target.kind;
      if (fresh.target.kind === 'new_session') {
        machineId = fresh.target.machine_id; path = fresh.target.workdir;
        executorMode = fresh.target.executor ? 'explicit' : 'route';
        taskType = fresh.target.route?.task_type ?? ''; level = fresh.target.route?.level ?? '';
        harness = fresh.target.executor?.harness ?? ''; modelId = fresh.target.executor?.model ?? '';
        effort = fresh.target.executor?.effort ?? '';
        presetId = fresh.target.preset ?? 'clean'; permission = fresh.target.permission_mode ?? '';
        groupId = fresh.target.group_id ?? '';
      } else sessionId = fresh.target.conversation_id;
      conflict = false; error = '';
      void refreshPreview();
    } catch { error = 'Не удалось обновить расписание. Повторите при восстановлении связи'; }
  }

  async function save() {
    if (saving) return;
    error = ''; conflict = false;
    const problem = validate();
    if (problem) { error = problem; return; }
    saving = true;
    const body = {
      name: name.trim(), cron: cron.trim(), timezone, prompt: prompt.trim(),
      target: buildTarget(), precheck: precheck.trim() || null,
      precheck_timeout_seconds: precheckTimeout ?? 60,
      catch_up_minutes: catchUp ?? 30, overlap, title_template: titleTemplate.trim() || '{name} · {date}'
    };
    try {
      if (schedule) {
        await patchSchedule(schedule.id, { ...body, base_revision: baseRevision }, opKey);
        await store.load();
        done(schedule.id);
      } else {
        const created = await createSchedule(body, opKey);
        await store.load();
        done(created.data.id);
      }
    } catch (err) {
      if (err instanceof ApiError && err.code === 'schedule_revision_conflict') {
        conflict = true;
      } else if (err instanceof ApiError && err.code === 'schedule_invalid' && err.details?.field === 'target') {
        targetError = err.message.trim() || 'Эту сессию нельзя выбрать целью';
      } else {
        error = errorText(err, 'Не удалось сохранить расписание. Повторите при восстановлении связи');
      }
    } finally { saving = false; }
  }

  function number(value: string): number | null {
    if (value.trim() === '') return null;
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
</script>

<section data-schedule-editor class="editor" aria-label="Редактор расписания">
  <a class="back" href="#/schedules" onclick={(event) => { event.preventDefault(); cancel(); }}>К расписанию</a>
  <h2>{schedule ? 'Правка расписания' : 'Новое расписание'}</h2>
  {#if conflict}
    <p class="error" role="alert">Расписание изменено с другого устройства. Обновите и повторите правку.</p>
    <Button onclick={() => void refresh()}>Обновить</Button>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="fields">
    <Field label="Название">
      {#snippet children({ id })}<input {id} bind:value={name} placeholder="Например, разбор коммитов" />{/snippet}
    </Field>

    <Segmented name="cron-preset" label="Шаблон расписания"
      options={[{ value: '', label: 'Своё' }, ...CRON_PRESETS.map((item) => ({ value: item.id, label: item.label }))]}
      value={CRON_PRESETS.find((item) => item.cron === cron)?.id ?? ''}
      onchange={(value) => pickPreset(value)} />
    <Field label="Cron (5 полей)" hint="Минута, час, день, месяц, день недели. Не чаще одного раза в 5 минут">
      {#snippet children({ id })}<input {id} bind:value={cron} class="mono" spellcheck={false} />{/snippet}
    </Field>
    <div class="preview" data-role="schedule-preview">
      <div class="preview-head"><span>Следующие 5 запусков · {timezone}</span><Button onclick={() => void refreshPreview()}>Показать</Button></div>
      {#if previewError}<p class="error" role="alert">{previewError}</p>{/if}
      {#if previewing}<p class="note">Считаю времена…</p>{/if}
      {#if preview.length > 0}
        <ul>{#each preview as item (item)}<li>{fireTime(item, timezone)}</li>{/each}</ul>
      {/if}
    </div>
    <Field label="Часовой пояс" hint="IANA-имя, например Europe/Moscow">
      {#snippet children({ id })}<input {id} bind:value={timezone} spellcheck={false} />{/snippet}
    </Field>

    <h3>Цель запуска</h3>
    <Segmented name="schedule-target" label="Цель запуска"
      options={[{ value: 'new_session', label: 'Новая сессия' }, { value: 'existing_session', label: 'Сообщение в сессию' }, { value: 'session_report', label: 'Отчёт из сессии' }]}
      bind:value={targetKind} />

    {#if targetKind === 'new_session'}
      <Select label="Машина" options={machineOptions} bind:value={machineId} />
      <WorkdirField {machineId} bind:value={path} />
      <Segmented name="schedule-executor" label="Исполнитель"
        options={[{ value: 'route', label: 'Маршрут' }, { value: 'explicit', label: 'Вручную' }]}
        bind:value={executorMode} />
      {#if executorMode === 'route'}
        <Select label="Тип задачи" options={taskOptions} bind:value={taskType} />
        <Select label="Уровень" options={levelOptions} bind:value={level} hint="Необязательно: влияет на выбор исполнителя политикой" />
      {:else}
        {#if harnesses.length > 0}
          <Segmented name="schedule-harness" label="Агент" options={harnesses.map((item) => ({ value: item.id, label: item.title }))} bind:value={harness} />
        {/if}
        <div class="pair">
          <Select label="Модель" options={modelOptions} bind:value={modelId} />
          <Select label="Усилие" options={effortOptions} bind:value={effort} />
        </div>
      {/if}
      <Select label="Инструкции" options={presetOptions} bind:value={presetId} hint={presets.find((item) => item.id === presetId)?.description} />
      {#if permissionOptions.length > 0}
        <Select label="Режим разрешений" options={permissionOptions} bind:value={permission} hint={permissionHint(permission, selectedHarness?.provider ?? 'claude')} />
      {/if}
      <Select label="Группа" options={groupOptions} bind:value={groupId} />
    {:else}
      <Select label="Сессия" options={sessionOptions} bind:value={sessionId} error={targetError || undefined}
        hint={targetKind === 'existing_session' ? 'Подсказка придёт в очередь выбранной сессии как обычный ввод' : undefined} />
    {/if}

    <Textarea label="Подсказка" bind:value={prompt} rows={4}
      hint={targetKind === 'session_report' ? 'Копия сессии только читает её историю и отвечает на этот промпт; ответ придёт карточкой в ленту сессии.' : undefined} />
    <Textarea label="Предпроверка" bind:value={precheck} rows={2} hint="Необязательная команда: ненулевой код отменяет запуск" mono />
    <div class="pair">
      <Field label="Таймаут предпроверки, с">
        {#snippet children({ id })}<input {id} type="number" min="1" max="300" value={precheckTimeout ?? ''} oninput={(event) => precheckTimeout = number(event.currentTarget.value)} />{/snippet}
      </Field>
      <Field label="Окно догона, мин.">
        {#snippet children({ id })}<input {id} type="number" min="0" max="1440" value={catchUp ?? ''} oninput={(event) => catchUp = number(event.currentTarget.value)} />{/snippet}
      </Field>
    </div>
    <Segmented name="schedule-overlap" label="Пересечения" options={[...OVERLAP_OPTIONS].map((item) => ({ value: item.value, label: item.label }))} bind:value={overlap} />
    <Field label={targetKind === 'session_report' ? 'Заголовок карточки' : 'Заголовок нового сеанса'} hint={targetKind === 'session_report' ? 'Карточка в ленте покажет его как «Рутина · …»; подставятся название и дата запуска' : 'В шаблоне подставятся название и дата запуска'}>
      {#snippet children({ id })}<input {id} bind:value={titleTemplate} />{/snippet}
    </Field>

    <div class="actions">
      <Button variant="primary" {...(saving ? { disabled: true as const, reason: 'Сохраняю…' } : { disabled: false as const })} onclick={() => void save()}>Сохранить</Button>
      <Button onclick={cancel}>Отмена</Button>
    </div>
  </div>
</section>

<style>
  .editor { display: grid; gap: var(--s4); }
  .back { min-height: var(--target); display: inline-flex; align-items: center; color: var(--accent-link); }
  h2 { margin: var(--s0); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  h3 { margin: var(--s4) var(--s0) var(--s0); font-size: var(--fs-body); }
  .error { margin: var(--s0); color: var(--danger); }
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .fields { display: grid; gap: var(--s5); }
  .pair { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr)); gap: var(--s4); }
  .mono { font-family: var(--font-mono); font-size: var(--fs-meta); }
  .preview { display: grid; gap: var(--s2); padding: var(--s5); border-radius: var(--radius-card); background: var(--surface); }
  .preview-head { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); color: var(--secondary); font-size: var(--fs-sm); }
  .preview ul { display: flex; flex-wrap: wrap; gap: var(--s2) var(--s5); margin: var(--s0); padding: var(--s0); list-style: none; }
  .preview li { color: var(--text); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
</style>
