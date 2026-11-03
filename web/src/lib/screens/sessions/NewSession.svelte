<script lang="ts">
  import { errorText } from '#lib/api/errors.js';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { loadConfig } from '#lib/api/config.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import { DEFAULT_PERMISSION_MODE, NO_MODELS_HINT, effortLabel, modelImagesNote, permissionHint, permissionLabel } from '#lib/format/labels.js';
  import Button from '#lib/ui/Button.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import { onMount, untrack } from 'svelte';
  import { defaultModelId, keptHarness, machineProblem, NewSessionModel, prefillWorkdir, soleEffort, soleMachine } from './newSessionModel';
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

  const machines = $derived(runtime ? [...runtime.machines.items.values()] : []);
  const machine = $derived(machines.find((item) => item.id === machineId));
  const home = $derived(machine?.home_dir ?? '');
  const harnesses = $derived(machine?.harnesses ?? []);
  const models = $derived(harnesses.find((item) => item.id === harness)?.models ?? []);
  const efforts = $derived(models.find((item) => item.id === modelId)?.efforts ?? []);
  const permissions = $derived(harnesses.find((item) => item.id === harness)?.permission_modes ?? []);
  const BUILTIN_PRESETS: PresetSummaryLike[] = [
    {
      id: 'clean',
      kind: 'clean',
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
  const provider = $derived(harnesses.find((item) => item.id === harness)?.provider ?? 'claude');
  const imagesNote = $derived(modelImagesNote(models.find((item) => item.id === modelId)?.images));
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
        limit_thresholds: { stale_after_secs: 2100, warn_pcts: [80], weekly_reserve_pct: 95 }
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

  async function send() {
    if (!model || !gate.ok) return;
    error = '';
    try {
      const created = await model.submit();
      const share = page.url.searchParams.get('share');
      if (share) await goto(resolve(`/share?id=${encodeURIComponent(share)}&conversation=${encodeURIComponent(created.conversationId)}` as '/share'));
      else await goto(resolve(`/c/${created.conversationId}` as '/'));
    } catch (err) {
      error = errorText(err, 'Не удалось создать сессию');
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
    <div class="pair">
      <Select label="Модель" options={modelOptions} bind:value={modelId} />
      <Select label="Усилие" options={effortOptions} bind:value={effort} />
    </div>
    {#if harness && models.length === 0}<p class="hint">{NO_MODELS_HINT}</p>{/if}
    {#if imagesNote}<p class="hint">{imagesNote}</p>{/if}
    {#if permissionOptions.length > 0}
      <Select label="Режим разрешений" options={permissionOptions} bind:value={permission} hint={permissionHint(permission, provider)} />
    {/if}
    <Select label="Инструкции" options={presetOptions} bind:value={profile} hint={selectedPreset?.description} />
    <Toggle block label="Кранч" bind:checked={crunch} />
    {#if crunch}<Textarea label="Текст продолжения" bind:value={resume} />{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if model?.errors.machine}<p class="error">{model.errors.machine}</p>{/if}
    {#if gate.ok}
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
