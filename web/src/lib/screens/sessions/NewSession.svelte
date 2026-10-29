<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { loadConfig } from '$lib/api/config';
  import type { Runtime } from '$lib/app/runtime';
  import { isTouchPrimary } from '$lib/composer/sendRules';
  import { browserRuntime } from '$lib/screens/chat/open';
  import Button from '$lib/ui/Button.svelte';
  import Segmented from '$lib/ui/Segmented.svelte';
  import Sheet from '$lib/ui/Sheet.svelte';
  import Toggle from '$lib/ui/Toggle.svelte';
  import { onMount } from 'svelte';
  import { machineProblem, NewSessionModel } from './newSessionModel';

  const touch = isTouchPrimary();
  let runtime = $state<Runtime | null>(null);
  let model = $state<NewSessionModel | null>(null);
  let machineId = $state('');
  let root = $state('');
  let sub = $state('');
  let harness = $state('');
  let modelId = $state('');
  let effort = $state('');
  let profile = $state('');
  let crunch = $state(false);
  let resume = $state('');
  let recent = $state<string[]>([]);
  let error = $state('');

  const machines = $derived(runtime ? [...runtime.machines.items.values()] : []);
  const machine = $derived(machines.find((item) => item.id === machineId));
  const roots = $derived(machine?.allowed_roots ?? []);
  const harnesses = $derived(machine?.harnesses ?? []);
  const models = $derived(harnesses.find((item) => item.id === harness)?.models ?? []);
  const efforts = $derived(models.find((item) => item.id === modelId)?.efforts ?? []);
  const workdir = $derived(join(root, sub));
  const gate = $derived.by(() => {
    if (!model) return { ok: false as const, reason: 'Открываю форму' };
    model.machineId = machineId;
    model.workdir = workdir;
    model.harness = harness;
    model.model = modelId;
    model.effort = effort;
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

  $effect(() => {
    const current = model;
    const id = machineId;
    if (!current || !id) return;
    current.machineId = id;
    void current.refreshRecent().then(() => {
      recent = current.recent;
    });
  });

  function join(base: string, extra: string): string {
    if (!base) return extra.startsWith('/') ? extra : '';
    const tail = extra.replace(/^\/+/, '');
    return tail ? `${base.replace(/\/$/, '')}/${tail}` : base;
  }

  async function send() {
    if (!model || !gate.ok) return;
    try {
      const created = await model.submit();
      await goto(resolve(`/c/${created.conversationId}` as '/'));
    } catch (err) {
      error = err instanceof Error ? err.message : 'Не удалось создать сессию';
    }
  }
</script>

{#snippet fields()}
  <label>Машина
    <select aria-label="Машина" bind:value={machineId}>
      <option value="">Выберите машину</option>
      {#each machines as item (item.id)}
        {@const problem = machineProblem(item)}
        <option value={item.id} disabled={Boolean(problem)}>{item.name}{problem ? ` · ${problem}` : ''}</option>
      {/each}
    </select>
  </label>
  <label>Рабочая папка
    <select aria-label="Корень" bind:value={root}>
      <option value="">Выберите корень</option>
      {#each roots as item (item)}<option value={item}>{item}</option>{/each}
      {#each recent as item (item)}<option value={item}>{item}</option>{/each}
    </select>
    <input aria-label="Подпуть" bind:value={sub} placeholder="подпуть" />
  </label>
  <Segmented name="harness" label="Агент" options={harnesses.map((item) => ({ value: item.id, label: item.title }))} bind:value={harness} />
  <label>Модель
    <select aria-label="Модель" bind:value={modelId}>
      <option value="">Выберите модель</option>
      {#each models as item (item.id)}<option value={item.id}>{item.title}</option>{/each}
    </select>
  </label>
  <label>Усилие
    <select aria-label="Усилие" bind:value={effort}>
      <option value="">Выберите усилие</option>
      {#each efforts as item (item)}<option value={item}>{item}</option>{/each}
    </select>
  </label>
  <label>Профиль инструкций
    <select aria-label="Профиль инструкций" bind:value={profile}>
      <option value="">Как у владельца</option>
      {#each machine?.profiles ?? [] as item (item.name)}<option value={item.name}>{item.title}</option>{/each}
    </select>
  </label>
  <Toggle label="Кранч" bind:checked={crunch} />
  {#if crunch}<label>Текст продолжения <textarea aria-label="Текст продолжения" bind:value={resume}></textarea></label>{/if}
  {#if error}<p>{error}</p>{/if}
  {#if model?.errors.machine}<p>{model.errors.machine}</p>{/if}
  {#if gate.ok}
    <Button variant="primary" onclick={send}>Создать</Button>
  {:else}
    <Button variant="primary" disabled reason={gate.reason} onclick={send}>Создать</Button>
  {/if}
{/snippet}

{#if touch}
  <Sheet open title="Новая сессия" onclose={() => goto(resolve('/sessions'))}>{@render fields()}</Sheet>
{:else}
  <dialog open aria-label="Новая сессия">
    <h2>Новая сессия</h2>
    {@render fields()}
  </dialog>
{/if}

<style>
  dialog {
    width: min(100%, var(--phone-max));
    margin: var(--s8) auto;
    border: var(--s0);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    padding: var(--s6);
  }
  label { display: flex; flex-direction: column; gap: var(--s2); margin-bottom: var(--s4); }
  select, input, textarea {
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--text);
    padding: var(--s4) var(--s6);
  }
  p { color: var(--danger); }
  h2 { margin: 0 0 var(--s4); font-size: var(--fs-title); }
</style>
