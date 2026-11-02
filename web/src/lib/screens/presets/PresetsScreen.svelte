<script lang="ts">
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { deletePreset } from '#lib/api/client.js';
  import { fmt } from '#lib/format/ru.js';
  import { ApiError } from '#lib/api/http.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import PresetEditor from './PresetEditor.svelte';
  import OwnerSheet from './OwnerSheet.svelte';
  import { formatOrigin, isAgentMarked, sortPresets } from './presetsModel.js';
  import '../secondary.css';

  let runtime = $state<Runtime | null>(null);
  let error = $state('');
  let creating = $state(false);
  let editing = $state<{ id: string; revision: number | null } | null>(null);
  let deleting = $state<{ id: string; title: string; used_by: number } | null>(null);
  let deleteError = $state('');
  let ownerOpen = $state(false);

  const presets = $derived(runtime ? sortPresets([...runtime.list.presets]) : []);
  const ownerMachines = $derived(
    runtime ? [...runtime.machines.items.values()].map((m) => ({ id: m.id, name: m.name })) : [],
  );

  async function load(): Promise<void> {
    error = '';
    try {
      runtime = await browserRuntime();
    } catch {
      error = 'Не удалось загрузить инструкции. Повторите при восстановлении связи';
    }
  }

  async function remove(): Promise<void> {
    const target = deleting;
    if (!target) return;
    deleteError = '';
    try {
      await deletePreset(target.id, crypto.randomUUID());
      deleting = null;
    } catch (err) {
      deleteError = err instanceof ApiError ? err.message : 'Не удалось удалить инструкцию';
    }
  }

  $effect(() => {
    void load();
  });
</script>

<main id="main" tabindex="-1" data-presets-ready={runtime ? '' : undefined}>
  <SecondaryHeader title="Инструкции" subtitle="Пресеты инструкций агента" />
  <div class="secondary-body"><div class="secondary-content">
    {#if error}
      <p class="error">{error}</p>
      <Button onclick={() => void load()}>Повторить</Button>
    {:else if !runtime}
      <p>Загружаю инструкции…</p>
    {:else}
      <Button variant="primary" onclick={() => (creating = true)}>Новая инструкция</Button>
      {#if deleteError}<p class="error" role="alert">{deleteError}</p>{/if}
      <ul>
        {#each presets as preset (preset.id)}
          <li data-preset={preset.id} data-owner-card={preset.kind === 'owner' ? '' : undefined}>
            <div class="row">
              <strong>{preset.title}</strong>
              {#if preset.kind !== 'custom'}
                <Pill color="neutral">встроенный</Pill>
              {:else if isAgentMarked(preset)}
                <span data-agent-mark><Pill color="accent">агент</Pill></span>
              {/if}
            </div>
            {#if preset.description}<p class="note">{preset.description}</p>{/if}
            {#if preset.kind === 'owner'}
              <div class="actions">
                <Button onclick={() => (ownerOpen = true)}>Посмотреть</Button>
              </div>
            {/if}
            {#if preset.kind === 'custom'}
              <p class="meta">версия {preset.revision}</p>
              <p class="meta">Используется в {preset.used_by} {fmt.plural(preset.used_by, ['сессии', 'сессиях', 'сессиях'])}</p>
              <p class="meta">{formatOrigin(preset)}</p>
              <div class="actions">
                <Button onclick={() => (editing = { id: preset.id, revision: preset.revision ?? null })}>Изменить</Button>
                <Button onclick={() => (deleting = { id: preset.id, title: preset.title, used_by: preset.used_by })}>Удалить</Button>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div></div>
</main>

{#if creating || editing}
  <PresetEditor
    preset={editing}
    onclose={() => {
      creating = false;
      editing = null;
    }}
    ondone={() => {
      creating = false;
      editing = null;
    }}
  />
{/if}

{#if deleting}
  <ConfirmDialog
    title={`Удалить инструкцию «${deleting.title}»?`}
    body={`Сессии, где он применён (${deleting.used_by}), продолжат работать со своей копией.`}
    confirmLabel="Удалить инструкцию"
    onconfirm={() => void remove()}
    onclose={() => (deleting = null)}
  />
{/if}

{#if ownerOpen && ownerMachines.length > 0}
  <OwnerSheet
    machines={ownerMachines}
    initialMachine={ownerMachines[0]?.id ?? ''}
    onclose={() => (ownerOpen = false)}
  />
{/if}

<style>
  ul {
    list-style: none;
    margin: var(--s6) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }
  li {
    background: var(--surface);
    border-radius: var(--radius-card);
    padding: var(--s6);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--s3);
  }
  strong {
    font-size: var(--fs-body);
  }
  .note {
    margin: var(--s2) 0 0;
    color: var(--secondary);
    font-size: var(--fs-meta);
  }
  .meta {
    margin: var(--s2) 0 0;
    color: var(--muted);
    font-size: var(--fs-meta);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3);
    margin-top: var(--s4);
  }
  .error {
    color: var(--danger);
  }
</style>
