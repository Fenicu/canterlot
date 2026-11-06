<script lang="ts">
  import { onMount } from 'svelte';
  import { flip } from 'svelte/animate';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { getRoutingPolicy, getSpawnWhitelist, listMachines, putRoutingPolicy } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { ApiError } from '#lib/api/http.js';
  import { effortLabel } from '#lib/format/labels.js';
  import { durationMs } from '#lib/motion.js';
  import { catalogTitles, pairName } from '#lib/routing/decision.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Select from '#lib/ui/Select.svelte';
  import ChipToggle from '#lib/ui/ChipToggle.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import RoutingPreview from './RoutingPreview.svelte';
  import { harnessOptions, mergeCatalog, modelOptions, type CatalogHarness, type WhitelistEntry } from './spawnWhitelist.js';
  import {
    LEVELS, MAX_CANDIDATES, MAX_TASK_TYPES, NOT_IN_WHITELIST, addCandidate, addTask, allowedByWhitelist, fromPolicy, isDirty,
    issuesAt, moveCandidate, removeCandidate, removeTask, toPolicy, uid, validate, type Draft, type DraftCandidate, type DraftTask, type Policy
  } from './routingPolicy.js';
  import '../secondary.css';

  type View = { policy?: Policy | null; revision: number; builtin: Policy };

  let runtime = $state<Runtime | null>(null);
  let view = $state<View | null>(null);
  let draft = $state<Draft | null>(null);
  let catalog = $state<CatalogHarness[]>([]);
  let machines = $state<{ harnesses: { id: string; models: { id: string; title: string }[] }[] }[]>([]);
  let whitelist = $state<WhitelistEntry[]>([]);
  let loadError = $state('');
  let saveError = $state('');
  let notice = $state('');
  let busy = $state(false);
  let conflict = $state(false);
  let confirmingReset = $state(false);
  let addErrors = $state<Record<string, string>>({});
  let picks = $state<Record<string, { harness: string; model: string }>>({});
  let excludeInput = $state<Record<string, string>>({});
  let unlimitedInput = $state('');

  const saved = $derived(view ? (view.policy ?? view.builtin) : null);
  const builtinActive = $derived(view !== null && (view.policy ?? null) === null);
  const issues = $derived(draft ? validate(draft) : []);
  const dirty = $derived(draft !== null && saved !== null && isDirty(draft, saved));
  const titles = $derived(catalogTitles(machines));
  const harnesses = $derived(harnessOptions(catalog));
  const firstPair = $derived({ harness: catalog[0]?.id ?? 'claude_code', model: 'default' });
  const pairOptions = $derived(
    draft
      ? [...new Set(draft.tasks.flatMap((task) => task.candidates.map((candidate) => `${candidate.harness}/${candidate.model}`)))]
      : []
  );

  function accept(next: View) {
    view = next;
    draft = fromPolicy(next.policy ?? next.builtin);
    conflict = false;
    saveError = '';
  }

  async function load() {
    loadError = '';
    try {
      const [policy, list, machineList] = await Promise.all([getRoutingPolicy(), getSpawnWhitelist(), listMachines()]);
      catalog = mergeCatalog(machineList.data);
      machines = machineList.data;
      whitelist = list.data.entries;
      accept(policy.data as View);
    } catch {
      loadError = 'Не удалось загрузить политику маршрутизации. Повторите при восстановлении связи';
    }
  }

  onMount(() => {
    void load();
    browserRuntime().then((rt) => (runtime = rt)).catch(() => undefined);
  });

  // Политику сохранили в другом окне: чистый черновик перечитывается, правки ждут решения владельца.
  $effect(() => {
    const revision = runtime?.list.routingRevision ?? null;
    if (revision === null || !view || revision === view.revision || busy) return;
    if (dirty) conflict = true;
    else void reloadQuietly();
  });

  async function reloadQuietly() {
    try {
      accept((await getRoutingPolicy()).data as View);
      notice = 'Политика обновлена из другого окна';
    } catch {
      // останется прежняя ревизия; сохранение ответит конфликтом
    }
  }

  async function send(policy: Policy | null) {
    if (!view || busy) return;
    busy = true;
    saveError = '';
    notice = '';
    try {
      const answer = await putRoutingPolicy({ policy, revision: view.revision }, crypto.randomUUID());
      accept(answer.data as View);
      notice = policy === null ? 'Действует встроенная политика' : 'Политика сохранена';
    } catch (err) {
      if (err instanceof ApiError && err.code === 'routing_policy_conflict') conflict = true;
      else saveError = errorText(err, 'Не удалось сохранить политику. Повторите при восстановлении связи');
    } finally {
      busy = false;
    }
  }

  function update(next: Draft | string, at: string) {
    if (typeof next === 'string') addErrors = { ...addErrors, [at]: next };
    else {
      addErrors = { ...addErrors, [at]: '' };
      draft = next;
    }
  }

  function pickFor(task: DraftTask): { harness: string; model: string } {
    return picks[task.uid] ?? firstPair;
  }

  function firstModel(harness: string): string | undefined {
    return catalog.find((item) => item.id === harness)?.models[0]?.id;
  }

  function allowed(candidate: DraftCandidate): boolean {
    return allowedByWhitelist(whitelist, candidate.harness, candidate.model, firstModel(candidate.harness));
  }

  /** Опции с текущим значением, даже если каталог машин его не знает. */
  function withCurrent(options: { value: string; label: string }[], value: string): { value: string; label: string }[] {
    return options.some((item) => item.value === value) ? options : [...options, { value, label: value }];
  }

  const EFFORTS = ['', 'low', 'medium', 'high', 'xhigh', 'max'];
  const effortOptions = EFFORTS.map((value) => ({ value, label: value ? effortLabel(value) : 'По уровню или по умолчанию' }));

  // Перетаскивание: указатель ведёт кандидата по списку, соседи едут следом (flip).
  let dragging = $state<{ task: string; candidate: string } | null>(null);
  function dragStart(event: PointerEvent, task: DraftTask, candidate: DraftCandidate) {
    if (event.button !== 0) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    dragging = { task: task.uid, candidate: candidate.uid };
    event.preventDefault();
  }
  function dragMove(event: PointerEvent) {
    if (!dragging || !draft) return;
    const task = draft.tasks.find((item) => item.uid === dragging!.task);
    if (!task) return;
    const rows = [...document.querySelectorAll<HTMLElement>(`[data-task="${task.uid}"] [data-candidate]`)];
    const from = task.candidates.findIndex((item) => item.uid === dragging!.candidate);
    let to = rows.findIndex((row) => {
      const box = row.getBoundingClientRect();
      return event.clientY < box.top + box.height / 2;
    });
    if (to < 0) to = rows.length - 1;
    else if (to > from) to -= 1;
    if (from >= 0 && to !== from) draft = moveCandidate(draft, task.uid, from, to);
  }
  function dragEnd() {
    dragging = null;
  }

  function addExclude(candidate: DraftCandidate) {
    const value = (excludeInput[candidate.uid] ?? '').trim();
    if (!value) return;
    if (!candidate.exclude.includes(value)) candidate.exclude = [...candidate.exclude, value];
    excludeInput = { ...excludeInput, [candidate.uid]: '' };
  }

  function addUnlimited() {
    const value = unlimitedInput.trim();
    if (!draft || !value) return;
    if (!draft.unlimited.includes(value)) draft.unlimited = [...draft.unlimited, value];
    unlimitedInput = '';
  }

  function numberOrNull(value: string): number | null {
    if (value.trim() === '') return null;
    const parsed = Number(value.replace(',', '.'));
    return Number.isFinite(parsed) ? parsed : Number.NaN;
  }

</script>

<main id="main" tabindex="-1" data-secondary-ready={view ? '' : undefined} data-routing-editor>
  <SecondaryHeader title="Маршрутизация" subtitle="Делегирование" back={{ path: '/settings', label: 'К настройкам' }} />
  <div class="secondary-body"><div class="secondary-content">
    {#if loadError}<p class="error" role="alert">{loadError}</p><button type="button" onclick={load}>Повторить</button>{/if}
    {#if !draft && !loadError}<p>Загружаю политику…</p>{/if}
    {#if draft && view}
      <p class="lead">Какой исполнитель берёт задачу каждого типа. Хаб идёт по кандидатам сверху вниз и берёт первого, кому хватает лимитов и свободных мест.</p>
      <p class="status" data-role="policy-status">
        {#if builtinActive}<Pill color="accent">Действует встроенная политика</Pill> Правки сохранятся как своя политика{:else}Своя политика · ревизия {view.revision}{/if}
      </p>
      {#if conflict}
        <div class="conflict" role="alert" data-role="policy-conflict">
          <p>Политику изменили в другом окне</p>
          <Button variant="primary" onclick={load}>Загрузить заново</Button>
          <p class="note">Ваши несохранённые правки пропадут</p>
        </div>
      {/if}

      <section aria-labelledby="types-title">
        <h2 id="types-title">Типы задач</h2>
        {#each draft.tasks as task (task.uid)}
          {@const taskIssues = issuesAt(issues, `task:${task.uid}`)}
          <article class="task" data-task={task.uid} data-task-key={task.key}>
            <div class="task-head">
              <label class="field"><span>Название</span><input type="text" bind:value={task.title} aria-label="Название типа {task.key}" /></label>
              <label class="field key"><span>Ключ</span><input type="text" bind:value={task.key} aria-label="Ключ типа {task.title}" spellcheck="false" autocapitalize="off" /></label>
            </div>
            {#each taskIssues as message (message)}<p class="error" role="alert">{message}</p>{/each}
            <ol class="candidates" aria-label="Кандидаты «{task.title}» по порядку">
              {#each task.candidates as candidate, index (candidate.uid)}
                {@const name = pairName(candidate.harness, candidate.model, titles)}
                {@const candidateIssues = issuesAt(issues, `candidate:${candidate.uid}`)}
                <li class="candidate" data-candidate={candidate.uid} data-pair="{candidate.harness}/{candidate.model}" class:dragging={dragging?.candidate === candidate.uid} animate:flip={{ duration: () => durationMs('--duration-row-fold') }}>
                  <div class="row">
                    <button type="button" class="grip" aria-label="Перетащить {name}" title="Перетащить" onpointerdown={(event) => dragStart(event, task, candidate)} onpointermove={dragMove} onpointerup={dragEnd} onpointercancel={dragEnd}><Icon name="grip" /></button>
                    <span class="order">{index + 1}</span>
                    <span class="name">{name}<small>{candidate.harness}{candidate.effort ? ` · ${effortLabel(candidate.effort).toLocaleLowerCase('ru-RU')}` : ''}</small></span>
                    {#if index > 0}<IconButton icon="up" label="Поднять {name}" onclick={() => (draft = moveCandidate(draft!, task.uid, index, index - 1))} />{:else}<IconButton icon="up" label="{name} уже первый" inert />{/if}
                    {#if index < task.candidates.length - 1}<IconButton icon="down" label="Опустить {name}" onclick={() => (draft = moveCandidate(draft!, task.uid, index, index + 1))} />{:else}<IconButton icon="down" label="{name} уже последний" inert />{/if}
                    <IconButton icon="trash" label="Убрать {name}" onclick={() => (draft = removeCandidate(draft!, task.uid, candidate.uid))} />
                  </div>
                  {#if !allowed(candidate)}<p class="warn" data-role="not-whitelisted"><Pill color="amber">{NOT_IN_WHITELIST}</Pill></p>{/if}
                  {#each candidateIssues as message (message)}<p class="error" role="alert">{message}</p>{/each}
                  <details class="more">
                    <summary>Настройки кандидата</summary>
                    <div class="grid">
                      <Select label="Агент" options={withCurrent(harnesses, candidate.harness)} bind:value={candidate.harness} />
                      <Select label="Модель" options={withCurrent(modelOptions(catalog, candidate.harness), candidate.model)} bind:value={candidate.model} />
                      <Select label="Усилие" options={effortOptions} bind:value={candidate.effort} />
                      <div class="levels" role="group" aria-label="Уровни задачи">
                        <span class="label">Уровни (пусто — любые)</span>
                        <div class="chips">
                          {#each LEVELS as level (level)}
                            <ChipToggle label={effortLabel(level)} checked={candidate.levels.includes(level)} onchange={(on) => (candidate.levels = on ? [...candidate.levels, level] : candidate.levels.filter((item) => item !== level))} />
                          {/each}
                        </div>
                      </div>
                      <div class="nudge">
                        <Segmented name="nudge-{candidate.uid}" label="Автопинок" options={[{ value: 'off', label: 'Выкл' }, { value: 'on', label: 'Вкл' }, { value: 'max', label: 'С пределом' }]} bind:value={candidate.nudge} />
                        {#if candidate.nudge === 'max'}
                          <label class="field small"><span>Толчков не больше</span><input type="number" inputmode="numeric" min="1" max="10" bind:value={candidate.maxNudges} /></label>
                        {/if}
                      </div>
                      <label class="field small"><span>Сторож, минут (пусто — без сторожа)</span><input type="number" inputmode="numeric" min="5" max="1440" value={candidate.watchdog ?? ''} oninput={(event) => (candidate.watchdog = numberOrNull(event.currentTarget.value))} /></label>
                      <div class="exclude">
                        <span class="label">Не брать для каталогов</span>
                        {#if candidate.exclude.length > 0}
                          <ul class="path-chips">
                            {#each candidate.exclude as path (path)}
                              <li><span>{path}</span><IconButton icon="close" label="Убрать каталог {path}" onclick={() => (candidate.exclude = candidate.exclude.filter((item) => item !== path))} /></li>
                            {/each}
                          </ul>
                        {/if}
                        <div class="inline">
                          <input type="text" placeholder="/srv/prod" aria-label="Каталог-исключение для {name}" bind:value={excludeInput[candidate.uid]} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); addExclude(candidate); } }} />
                          <Button onclick={() => addExclude(candidate)}>Добавить</Button>
                        </div>
                      </div>
                    </div>
                  </details>
                </li>
              {/each}
            </ol>
            <div class="add">
              <Select label="Агент" options={harnesses} value={pickFor(task).harness} onchange={(event) => (picks = { ...picks, [task.uid]: { harness: event.currentTarget.value, model: 'default' } })} />
              <Select label="Модель" options={modelOptions(catalog, pickFor(task).harness)} value={pickFor(task).model} onchange={(event) => (picks = { ...picks, [task.uid]: { ...pickFor(task), model: event.currentTarget.value } })} />
              {#if task.candidates.length >= MAX_CANDIDATES}
                <Button disabled reason="Не больше {MAX_CANDIDATES} кандидатов">Добавить кандидата</Button>
              {:else}
                <Button onclick={() => update(addCandidate(draft!, task.uid, pickFor(task)), task.uid)}>Добавить кандидата</Button>
              {/if}
            </div>
            {#if addErrors[task.uid]}<p class="error" role="alert">{addErrors[task.uid]}</p>{/if}
            <div class="task-foot">
              <Button variant="text" onclick={() => (draft = removeTask(draft!, task.uid))}>Удалить тип «{task.title || task.key}»</Button>
            </div>
          </article>
        {/each}
        {#each issuesAt(issues, 'tasks') as message (message)}<p class="error" role="alert">{message}</p>{/each}
        {#if draft.tasks.length >= MAX_TASK_TYPES}
          <Button disabled reason="Не больше {MAX_TASK_TYPES} типов задач">Добавить тип задачи</Button>
        {:else}
          <Button onclick={() => update(addTask(draft!, firstPair), 'tasks')}>Добавить тип задачи</Button>
        {/if}
        {#if addErrors.tasks}<p class="error" role="alert">{addErrors.tasks}</p>{/if}
      </section>

      <section aria-labelledby="providers-title">
        <h2 id="providers-title">Пороги провайдеров</h2>
        <p class="note">Кандидат пропускается, если расход окна выше порога. Пусто — без порога</p>
        <div class="providers">
          {#each draft.providers as provider (provider.provider)}
            <fieldset data-provider={provider.provider}>
              <legend>{pairName(provider.provider === 'claude' ? 'claude_code' : provider.provider, 'default')}</legend>
              <label class="field small"><span>Неделя, %</span><input type="number" inputmode="numeric" min="0" max="100" value={provider.weekly ?? ''} oninput={(event) => (provider.weekly = numberOrNull(event.currentTarget.value))} /></label>
              <label class="field small"><span>5 часов, %</span><input type="number" inputmode="numeric" min="0" max="100" value={provider.fiveHour ?? ''} oninput={(event) => (provider.fiveHour = numberOrNull(event.currentTarget.value))} /></label>
              <label class="field small"><span>Месяц, %</span><input type="number" inputmode="numeric" min="0" max="100" value={provider.monthly ?? ''} oninput={(event) => (provider.monthly = numberOrNull(event.currentTarget.value))} /></label>
              {#each issuesAt(issues, `provider:${provider.provider}`) as message (message)}<p class="error" role="alert">{message}</p>{/each}
            </fieldset>
          {/each}
        </div>
      </section>

      <section aria-labelledby="parallel-title">
        <h2 id="parallel-title">Параллельность</h2>
        <p class="note">Сколько делегированных сессий одной пары работают одновременно. Лишние ждут в очереди запуска</p>
        <ul class="plain">
          {#each draft.parallel as item (item.uid)}
            <li class="parallel" data-parallel={item.pair}>
              <label class="field"><span>Пара</span><input type="text" list="routing-pairs" bind:value={item.pair} spellcheck="false" autocapitalize="off" /></label>
              <label class="field small"><span>Не больше</span><input type="number" inputmode="numeric" min="1" bind:value={item.limit} /></label>
              <IconButton icon="trash" label="Убрать предел {item.pair}" onclick={() => (draft!.parallel = draft!.parallel.filter((known) => known.uid !== item.uid))} />
              {#each issuesAt(issues, `parallel:${item.uid}`) as message (message)}<p class="error" role="alert">{message}</p>{/each}
            </li>
          {/each}
        </ul>
        <datalist id="routing-pairs">{#each pairOptions as pair (pair)}<option value={pair}></option>{/each}</datalist>
        <Button onclick={() => (draft!.parallel = [...draft!.parallel, { uid: uid('p'), pair: pairOptions[0] ?? '', limit: 1 }])}>Добавить предел</Button>
      </section>

      <section aria-labelledby="unlimited-title">
        <h2 id="unlimited-title">Модели без лимитов</h2>
        <p class="note">Для этих моделей пороги провайдера не проверяются</p>
        {#if draft.unlimited.length > 0}
          <ul class="path-chips">
            {#each draft.unlimited as model (model)}
              <li><span>{model}</span><IconButton icon="close" label="Убрать {model}" onclick={() => (draft!.unlimited = draft!.unlimited.filter((item) => item !== model))} /></li>
            {/each}
          </ul>
        {/if}
        <div class="inline">
          <input type="text" placeholder="opencode-go/beam-free" aria-label="Модель без лимитов" bind:value={unlimitedInput} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); addUnlimited(); } }} />
          <Button onclick={addUnlimited}>Добавить</Button>
        </div>
      </section>

      <section aria-labelledby="unknown-title">
        <h2 id="unknown-title">Неизвестные лимиты</h2>
        <Segmented name="unknown-limits" label="Неизвестные лимиты" options={[{ value: 'allow', label: 'Пропускать кандидата дальше' }, { value: 'skip', label: 'Не брать кандидата' }]} bind:value={draft.unknownLimits} />
        <p class="note">Когда показания лимитов устарели или их нет</p>
      </section>

      <section aria-labelledby="guard-title">
        <h2 id="guard-title">Страж машины</h2>
        <p class="note" data-role="guard-note">Пороги стража (свободный диск, нагрузка, память) задаются в hub.toml и здесь не меняются; хаб их не отдаёт. Вердикт стража по текущим метрикам показывает проверка ниже</p>
      </section>

      <div class="actions" class:sticky={dirty} data-role="policy-actions">
        {#if saveError}<p class="error" role="alert">{saveError}</p>{/if}
        {#if notice && !dirty}<p class="note" role="status">{notice}</p>{/if}
        {#if busy}
          <Button variant="primary" disabled reason="Сохраняю…">Сохранить</Button>
        {:else if !dirty}
          <Button variant="primary" disabled reason="Нет изменений">Сохранить</Button>
        {:else if issues.length > 0}
          <Button variant="primary" disabled reason="Исправьте ошибки выше: {issues.length}">Сохранить</Button>
        {:else}
          <Button variant="primary" onclick={() => void send(toPolicy(draft!))}>Сохранить</Button>
        {/if}
        {#if dirty && !busy}<Button onclick={() => saved && (draft = fromPolicy(saved))}>Отменить правки</Button>{/if}
        {#if builtinActive}
          <Button disabled reason="Уже действует встроенная политика">Сбросить к встроенной</Button>
        {:else}
          <Button onclick={() => (confirmingReset = true)}>Сбросить к встроенной</Button>
        {/if}
      </div>

      <RoutingPreview types={Object.entries(saved?.task_types ?? {}).map(([key, task]) => ({ key, title: task.title }))} {titles} stale={dirty} />
    {/if}
  </div></div>
</main>

{#if confirmingReset}
  <ConfirmDialog
    title="Сбросить к встроенной политике?"
    body="Своя политика будет удалена, маршрут пойдёт по встроенной раскладке"
    confirmLabel="Сбросить"
    danger
    onconfirm={() => { confirmingReset = false; void send(null); }}
    onclose={() => (confirmingReset = false)}
  />
{/if}

<style>
  section { margin-bottom: var(--s10); }
  .lead { color: var(--secondary); }
  .status { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); color: var(--secondary); font-size: var(--fs-meta); }
  .conflict { display: grid; gap: var(--s4); justify-items: start; padding: var(--s6) var(--s8); border-radius: var(--radius-card); background: var(--warning-soft); }
  .conflict p, .conflict .note { margin: var(--s0); color: var(--warning); }
  .task { display: grid; gap: var(--s4); margin-bottom: var(--s8); padding: var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  .task-head { display: grid; grid-template-columns: minmax(0, 2fr) minmax(0, 1fr); gap: var(--s4); }
  .field { display: grid; gap: var(--s2); min-width: 0; }
  .field > span, .label { color: var(--secondary); font-size: var(--fs-meta); }
  .field.small input { inline-size: calc(4ch + 2 * var(--s8)); max-inline-size: 100%; font-variant-numeric: tabular-nums; appearance: textfield; -moz-appearance: textfield; }
  .field.small input::-webkit-inner-spin-button, .field.small input::-webkit-outer-spin-button { appearance: none; margin: 0; }
  input { min-width: 0; }
  .candidates { display: grid; gap: var(--s3); margin: var(--s0); padding: var(--s0); list-style: none; }
  .candidate { --field-bg: var(--surface); display: grid; gap: var(--s2); padding: var(--s3) var(--s4); border-radius: var(--radius-card); background: var(--raised); }
  .candidate.dragging { background: var(--selected); }
  .row { display: flex; align-items: center; gap: var(--s2); min-width: 0; }
  .grip { display: inline-flex; align-items: center; justify-content: center; min-width: var(--target); min-height: var(--target); padding: var(--s0); background: var(--transparent); color: var(--muted); cursor: grab; touch-action: none; }
  .order { min-width: var(--s8); color: var(--muted); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .name { flex: 1; display: grid; min-width: 0; overflow-wrap: anywhere; }
  .name small { color: var(--muted); font-size: var(--fs-sm); }
  .row :global(button[aria-disabled]) { color: var(--muted); }
  .warn, .error { margin: var(--s0); font-size: var(--fs-sm); }
  .more summary { min-height: var(--target); display: flex; align-items: center; color: var(--accent-link); font-size: var(--fs-meta); cursor: pointer; }
  .grid { display: grid; gap: var(--s6); padding-block: var(--s4); }
  .chips { display: flex; flex-wrap: wrap; gap: var(--s2); }
  .levels, .exclude, .nudge { display: grid; gap: var(--s3); }
  .path-chips { display: flex; flex-wrap: wrap; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .path-chips li { display: inline-flex; align-items: center; gap: var(--s1); padding-left: var(--s6); border-radius: var(--radius-pill); background: var(--chat-bg); font-family: var(--font-mono); font-size: var(--fs-sm); }
  .path-chips :global(button) { padding: var(--s0); background: var(--transparent); }
  .inline { display: flex; flex-wrap: wrap; gap: var(--s3); align-items: center; }
  .inline input { flex: 1 1 var(--bar-56); min-height: var(--target); padding: var(--s5) var(--s8); border: 0; border-radius: var(--radius-pill); background: var(--field-bg, var(--raised)); font-size: var(--fs-input); }
  .add { display: grid; gap: var(--s4); }
  .task-foot { display: flex; justify-content: flex-end; color: var(--danger); }
  .providers { display: grid; gap: var(--s4); }
  fieldset { display: flex; flex-wrap: wrap; gap: var(--s4); margin: var(--s0); padding: var(--s4) var(--s6) var(--s6); border: var(--s0); border-radius: var(--radius-card); background: var(--surface); }
  legend { float: left; width: 100%; padding: var(--s0); font-weight: var(--weight-medium); }
  .plain { display: grid; gap: var(--s3); margin: var(--s0) var(--s0) var(--s4); padding: var(--s0); list-style: none; }
  .parallel { display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--s4); padding: var(--s4) var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  .parallel .field:first-child { flex: 1 1 var(--bar-56); }
  .parallel .error { flex-basis: 100%; }
  .actions { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--s4); margin: var(--s0) calc(-1 * var(--s8)) var(--s10); padding: var(--s6) var(--s8); background: var(--bg); }
  /* С правками кнопка сохранения всегда под рукой: панель липнет к низу и закрывает отступ прокрутки. */
  .actions.sticky { position: sticky; bottom: calc(-1 * var(--s8)); z-index: 1; padding-bottom: calc(var(--s6) + var(--s8)); }
  .actions .error, .actions .note { flex-basis: 100%; margin: var(--s0); }
  .actions :global(.reason) { margin-block: var(--s0); }
  @media (min-width: 700px) { .add { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto; align-items: end; } .grid { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); } .levels, .exclude, .nudge { grid-column: 1 / -1; } }
</style>
