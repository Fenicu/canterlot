<script lang="ts">
  import { errorText } from '#lib/api/errors.js';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { loadConfig } from '#lib/api/config.js';
  import { claudeHistories } from '#lib/api/client.js';
  import { fmt } from '#lib/format/ru.js';
  import { limitThresholds } from '#lib/stores/limitThresholds.svelte.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import { browserRuntime } from '#lib/app/browser.js';
  import { ANTIGRAVITY_CATALOG_NOTE, DEFAULT_PERMISSION_MODE, NO_MODELS_HINT, effortLabel, modelImagesNote, permissionHint, permissionLabel } from '#lib/format/labels.js';
  import Button from '#lib/ui/Button.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import { onMount, untrack } from 'svelte';
  import { defaultModelId, historySize, keptHarness, machineProblem, NewSessionModel, prefillWorkdir, soleEffort, soleMachine } from './newSessionModel';
  import WorkdirField from './WorkdirField.svelte';
  import { sortPresets, type PresetSummaryLike } from '../presets/presetsModel.js';

  const touch = isTouchPrimary();
  let runtime = $state<Runtime | null>(null);
  let model = $state<NewSessionModel | null>(null);
  let machineId = $state('');
  let path = $state('');
  let harness = $state('');
  let modelId = $state('');
  let effort = $state('');
  let permission = $state('');
  let profile = $state('clean');
  let crunch = $state(false);
  let resume = $state('');
  let recent = $state<string[]>([]);
  let error = $state('');
  let resumeOpen = $state(false);
  let resumeLoading = $state(false);
  let resumeError = $state('');
  let resumeItems = $state<{ id: string; modified_at_ms: number; size: number; preview: string }[]>([]);
  let resumeNativeId = $state('');
  let resumeFor = $state('');

  const machines = $derived(runtime ? [...runtime.machines.items.values()] : []);
  const machine = $derived(machines.find((item) => item.id === machineId));
  const home = $derived(machine?.home_dir ?? '');
  const harnesses = $derived(machine?.harnesses ?? []);
  const selectedHarness = $derived(harnesses.find((item) => item.id === harness));
  const provider = $derived(selectedHarness?.provider ?? 'claude');
  const models = $derived(selectedHarness?.models ?? []);
  const efforts = $derived(models.find((item) => item.id === modelId)?.efforts ?? []);
  const permissions = $derived((selectedHarness?.permission_modes ?? []).filter((mode) => !(provider === 'antigravity' && mode === 'read_only')));
  const BUILTIN_PRESETS: PresetSummaryLike[] = [
    {
      id: 'clean',
      kind: 'clean',
      purpose: 'instructions',
      title: 'Без инструкций',
      description: 'Агент запускается без личных инструкций владельца: без его глобальных CLAUDE.md и AGENTS.md и без автопамяти. Инструкции самого репозитория, вход, MCP-серверы и плагины владельца остаются.',
      revision: null,
      text_bytes: null,
      created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
      last_edit: null,
      used_by: 0
    },
    {
      id: 'owner',
      kind: 'owner',
      purpose: 'instructions',
      title: 'Как у владельца',
      description: 'Конфигурация агента владельца без изменений: его CLAUDE.md, AGENTS.md и автопамять.',
      revision: null,
      text_bytes: null,
      created: { by: 'builtin', conversation_id: null, conversation_title: null, at: null },
      last_edit: null,
      used_by: 0
    }
  ];
  const presets = $derived(runtime && runtime.list.presets.length > 0 ? sortPresets([...runtime.list.presets]) : BUILTIN_PRESETS);
  const selectedPreset = $derived(presets.find((item) => item.id === profile) ?? BUILTIN_PRESETS[0]);
  const machineOptions = $derived([
    { value: '', label: 'Выберите машину' },
    ...machines.map((item) => {
      const problem = machineProblem(item);
      return { value: item.id, label: `${item.name}${problem ? ` · ${problem}` : ''}`, disabled: Boolean(problem) };
    })
  ]);
  const modelOptions = $derived([{ value: '', label: 'Выбрать' }, ...models.map((item) => ({ value: item.id, label: item.title }))]);
  const effortOptions = $derived([{ value: '', label: 'Выбрать' }, ...efforts.map((item) => ({ value: item, label: effortLabel(item) }))]);
  const presetOptions = $derived(presets.map((item) => ({ value: item.id, label: item.title })));
  const permissionOptions = $derived(permissions.map((value) => ({ value, label: permissionLabel(value) })));
  const canResume = $derived(
    Boolean(
      machine?.capabilities?.includes('claude_histories_v1') &&
        (selectedHarness?.id === 'claude_code' || selectedHarness?.provider === 'claude')
    )
  );
  const imagesNote = $derived(modelImagesNote(models.find((item) => item.id === modelId)?.images));
  const catalogNote = $derived(provider === 'antigravity' ? ANTIGRAVITY_CATALOG_NOTE : '');
  const gate = $derived.by(() => {
    if (!model) return { ok: false as const, reason: 'Открываю форму' };
    model.machineId = machineId;
    model.workdir = path;
    model.harness = harness;
    model.model = modelId;
    model.effort = effort;
    model.permission = permission;
    model.profile = profile;
    model.crunch = crunch;
    model.resumeText = resume;
    model.resumeNativeId = resumeNativeId;
    return model.canSubmit;
  });

  onMount(() => {
    let alive = true;
    void browserRuntime().then(async (value) => {
      const config = await loadConfig().catch(() => null);
      if (!alive) return;
      runtime = value;
      const fallback = {
        defaults: { crunch_resume_text: 'продолжи с места остановки' },
        limit_thresholds: limitThresholds
      };
      model = new NewSessionModel(value.machines, (config ?? fallback) as never, value.journal);
      resume = model.resumeText;
    });
    return () => {
      alive = false;
    };
  });

  // Другой агент может не знать выбранный режим: тогда снова действует значение по умолчанию.
  $effect(() => {
    const modes = permissions;
    if (modes.length === 0 || modes.includes(permission)) return;
    permission = modes.includes(DEFAULT_PERMISSION_MODE) ? DEFAULT_PERMISSION_MODE : (modes[0] ?? '');
  });

  $effect(() => {
    const current = model;
    const id = machineId;
    if (!current) return;
    current.machineId = id;
    void current.refreshRecent().then(() => {
      recent = current.recent;
    });
  });

  // Каталог приходит позже формы: подбор живёт в эффектах и срабатывает, когда данные появятся.
  $effect(() => {
    const only = soleMachine(machines);
    if (only && !machineId) machineId = only;
  });

  // Другая машина, агент или папка — другая история: выбранная не переезжает молча.
  let resumeKey = '';
  $effect(() => {
    const key = `${machineId}|${harness}|${path}`;
    untrack(() => {
      if (resumeKey === key) return;
      resumeKey = key;
      resumeNativeId = '';
      resumeItems = [];
      resumeFor = '';
      resumeError = '';
    });
  });

  $effect(() => {
    if (!resumeOpen || !canResume || !machineId || !path) return;
    const key = `${machineId}|${path.trim().replace(/(?<=.)\/+$/, '')}`;
    if (resumeFor === key) return;
    resumeFor = key;
    void loadHistories();
  });

  async function loadHistories() {
    const target = machineId;
    const workdir = path.trim().replace(/(?<=.)\/+$/, '');
    if (!target || !workdir) return;
    resumeLoading = true;
    resumeError = '';
    try {
      const response = await claudeHistories(target, workdir);
      if (machineId !== target || path.trim().replace(/(?<=.)\/+$/, '') !== workdir) return;
      resumeItems = response.data.histories;
    } catch {
      resumeError = 'Не удалось загрузить истории Claude Code';
    } finally {
      resumeLoading = false;
    }
  }

  function chooseHistory(id: string) {
    resumeNativeId = resumeNativeId === id ? '' : id;
  }

  let prefilled = '';
  $effect(() => {
    const next = home;
    untrack(() => {
      path = prefillWorkdir(path, prefilled, next);
    });
    prefilled = next;
  });

  $effect(() => {
    const list = harnesses;
    untrack(() => {
      harness = keptHarness(list, harness);
    });
  });

  $effect(() => {
    const list = models;
    untrack(() => {
      modelId = defaultModelId(list, modelId);
    });
  });

  $effect(() => {
    const list = efforts;
    untrack(() => {
      effort = soleEffort(list, effort);
    });
  });

  let creating = $state(false);

  async function send() {
    if (!model || !gate.ok || creating) return;
    error = '';
    creating = true;
    try {
      const created = await model.submit();
      const share = page.url.searchParams.get('share');
      if (share) await goto(resolve(`/share?id=${encodeURIComponent(share)}&conversation=${encodeURIComponent(created.conversationId)}` as '/share'));
      else await goto(resolve(`/c/${created.conversationId}` as '/'));
    } catch (err) {
      error = errorText(err, 'Не удалось создать сессию');
    } finally {
      creating = false;
    }
  }
</script>

{#snippet fields()}
  <div class="fields">
    <Select label="Машина" options={machineOptions} bind:value={machineId} />
    {#if recent.length > 0}
      <div role="group" aria-label="Недавние папки" class="recent">
        <p class="hint">Недавние папки</p>
        {#each recent as item (item)}
          <button type="button" class="recent-item" onclick={() => { path = item; }}>{item}</button>
        {/each}
      </div>
    {/if}
    <WorkdirField {machineId} bind:value={path} />
    <Segmented name="harness" label="Агент" options={harnesses.map((item) => ({ value: item.id, label: item.title }))} bind:value={harness} />
    {#if canResume}
      <details class="resume" bind:open={resumeOpen} data-role="claude-history">
        <summary>Продолжить историю Claude Code</summary>
        {#if resumeLoading}
          <p class="hint">Загружаю истории…</p>
        {:else if resumeError}
          <p class="error" role="alert">{resumeError}</p>
        {:else if resumeItems.length === 0}
          <p class="hint">В этой папке нет историй Claude Code</p>
        {:else}
          <ul class="histories">
            {#each resumeItems as item (item.id)}
              <li>
                <button
                  type="button"
                  class="history"
                  class:selected={resumeNativeId === item.id}
                  aria-pressed={resumeNativeId === item.id}
                  onclick={() => chooseHistory(item.id)}
                >
                  <span class="when">{fmt.stamp(item.modified_at_ms, Date.now())}</span>
                  <span class="size">{historySize(item.size)}</span>
                  <span class="preview">{item.preview || 'Без первого сообщения'}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </details>
    {/if}
    {#if provider === 'antigravity'}
      <Select label="Модель" options={modelOptions} bind:value={modelId} />
    {:else}
      <div class="pair">
        <Select label="Модель" options={modelOptions} bind:value={modelId} />
        <Select label="Усилие" options={effortOptions} bind:value={effort} />
      </div>
    {/if}
    {#if harness && models.length === 0}<p class="hint">{NO_MODELS_HINT}</p>{/if}
    {#if catalogNote}<p class="hint">{catalogNote}</p>{/if}
    {#if imagesNote}<p class="hint">{imagesNote}</p>{/if}
    {#if permissionOptions.length > 0}
      <Select label="Режим разрешений" options={permissionOptions} bind:value={permission} hint={permissionHint(permission, provider)} />
    {/if}
    <Select label="Инструкции" options={presetOptions} bind:value={profile} hint={selectedPreset?.description} />
    <Toggle block label="Кранч" bind:checked={crunch} />
    {#if crunch}<Textarea label="Текст продолжения" bind:value={resume} />{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if model?.errors.machine}<p class="error">{model.errors.machine}</p>{/if}
    {#if creating}
      <Button variant="primary" full disabled reason="Сессия создаётся" onclick={send}>Создаю…</Button>
    {:else if gate.ok}
      <Button variant="primary" full onclick={send}>Создать</Button>
    {:else}
      <Button variant="primary" full disabled reason={gate.reason} onclick={send}>Создать</Button>
    {/if}
    {#if !touch}<Button variant="text" full onclick={() => goto(resolve('sessions'))}>Отмена</Button>{/if}
  </div>
{/snippet}

{#if touch}
  <Sheet
    open
    title="Новая сессия"
    onclose={() => goto(resolve('sessions'))}
  >{@render fields()}</Sheet>
{:else}
  <dialog open aria-label="Новая сессия">
    <h2>Новая сессия</h2>
    {@render fields()}
  </dialog>
{/if}

<style>
  dialog {
    position: static;
    flex-shrink: 0;
    width: min(100%, var(--phone-max));
    margin: var(--s8) auto;
    border: var(--s0);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    padding: var(--s6) var(--s8) var(--s8);
  }
  .fields { display: flex; flex-direction: column; gap: var(--s6); }
  .pair { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr)); gap: var(--s4); }
  .error { margin: 0; color: var(--danger); }
  .hint { color: var(--muted); font-size: var(--fs-sm); margin: 0; }
  .recent { display: flex; flex-direction: column; gap: var(--s1); }
  .resume { display: flex; flex-direction: column; gap: var(--s3); }
  .resume summary {
    min-height: var(--target);
    cursor: pointer;
    color: var(--secondary);
    font-size: var(--fs-sm);
  }
  .histories { list-style: none; display: flex; flex-direction: column; gap: var(--s2); margin: 0; padding: 0; }
  .history {
    display: grid;
    grid-template-columns: max-content max-content minmax(0, 1fr);
    gap: var(--s1) var(--s4);
    width: 100%;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-card);
    background: var(--transparent);
    color: var(--text);
    padding: var(--s3) var(--s4);
    text-align: left;
    align-items: baseline;
  }
  .history.selected { background: var(--raised); outline: var(--s0) solid var(--accent); outline-offset: calc(-1 * var(--s0)); }
  .history .when, .history .size { color: var(--muted); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .history .preview { grid-column: 1 / -1; color: var(--secondary); font-size: var(--fs-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .recent-item {
    display: block;
    width: 100%;
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--transparent);
    color: var(--secondary);
    padding: var(--s2) var(--s4);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  h2 { margin: 0 0 var(--s4); font-size: var(--fs-title); }
</style>
