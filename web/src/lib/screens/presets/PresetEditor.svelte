<script lang="ts">
  import { errorText } from '#lib/api/errors.js';
  import { createPreset, getPreset, patchPreset } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Disclosure from '#lib/ui/Disclosure.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import {
    byteCounter,
    PRESET_NUDGE_TEXT_LIMIT,
    PRESET_TEXT_LIMIT,
    textBytes,
  } from './presetsModel.js';

  type Detail = components['schemas']['Preset'];

  let {
    preset,
    purpose = 'instructions',
    onclose,
    ondone
  }: {
    preset: { id: string; revision: number | null } | null;
    purpose?: 'instructions' | 'nudge';
    onclose: () => void;
    ondone: () => void;
  } = $props();

  const limit = $derived(
    purpose === 'nudge' ? PRESET_NUDGE_TEXT_LIMIT : PRESET_TEXT_LIMIT,
  );
  const noun = $derived(purpose === 'nudge' ? 'текст толчка' : 'инструкцию');
  const editTitle = $derived(
    purpose === 'nudge' ? 'Изменить текст толчка' : 'Изменить инструкцию',
  );
  const newTitle = $derived(
    purpose === 'nudge' ? 'Новый текст толчка' : 'Новая инструкция',
  );

  const editing = $derived(preset !== null);
  let title = $state('');
  let description = $state('');
  let text = $state('');
  let baseRevision = $state(0);
  let serverText = $state('');
  let loaded = $state(false);
  let saving = $state(false);
  let titleError = $state<string | null>(null);
  let descriptionError = $state<string | null>(null);
  let textError = $state<string | null>(null);
  let conflict = $state<{ current: number } | null>(null);
  let draft = $state<string | null>(null);
  let loadError = $state<string | null>(null);

  const bytes = $derived(textBytes(text));
  const counter = $derived(byteCounter(bytes, limit));
  const canSave = $derived(
    loaded && !saving && title.trim().length > 0 && text.trim().length > 0 && bytes <= limit
  );

  async function load(): Promise<void> {
    if (!preset) {
      loaded = true;
      return;
    }
    loaded = false;
    loadError = null;
    try {
      const response = await getPreset(preset.id, purpose);
      const detail = response.data as Detail;
      title = detail.title;
      description = detail.description;
      text = detail.text ?? '';
      serverText = text;
      baseRevision = detail.revision ?? 0;
    } catch {
      loadError = `Не удалось загрузить ${noun}. Закройте и попробуйте снова`;
    } finally {
      loaded = true;
    }
  }

  $effect(() => {
    void preset;
    title = '';
    description = '';
    text = '';
    serverText = '';
    baseRevision = 0;
    titleError = null;
    descriptionError = null;
    textError = null;
    conflict = null;
    draft = null;
    loadError = null;
    void load();
  });

  function applyError(err: unknown): void {
    titleError = null;
    descriptionError = null;
    textError = null;
    if (err instanceof ApiError && err.code === 'preset_revision_conflict') {
      conflict = { current: (err.details?.current_revision as number | undefined) ?? baseRevision + 1 };
      draft = text;
      return;
    }
    if (err instanceof ApiError && err.code === 'preset_title_taken') {
      titleError = 'Инструкции с таким названием уже есть';
      return;
    }
    if (err instanceof ApiError && err.code === 'preset_invalid') {
      const field = err.details?.field as string | undefined;
      const message = err.message || 'Некорректные инструкции';
      if (field === 'description') descriptionError = message;
      else if (field === 'text') textError = message;
      else titleError = message;
      return;
    }
    textError = errorText(err, 'Не удалось сохранить инструкцию');
  }

  async function save(): Promise<void> {
    if (!canSave || saving) return;
    saving = true;
    titleError = null;
    descriptionError = null;
    textError = null;
    try {
      const body = { title: title.trim(), description: description.trim(), text };
      if (preset) {
        await patchPreset(
          preset.id,
          { ...body, base_revision: baseRevision },
          crypto.randomUUID(),
          purpose,
        );
      } else {
        await createPreset({ ...body, purpose }, crypto.randomUUID());
      }
      ondone();
    } catch (err) {
      applyError(err);
    } finally {
      saving = false;
    }
  }

  async function refresh(): Promise<void> {
    if (!preset) return;
    try {
      const response = await getPreset(preset.id, purpose);
      const detail = response.data as Detail;
      baseRevision = detail.revision ?? baseRevision + 1;
      serverText = detail.text ?? '';
      title = detail.title;
      description = detail.description;
      text = serverText;
      conflict = null;
      draft = null;
    } catch {
      loadError = 'Не удалось обновить инструкцию. Попробуйте снова';
    }
  }
</script>

<Sheet open title={editing ? editTitle : newTitle} {onclose}>
  {#if editing && !loaded}
    <p>Загружаю инструкцию…</p>
  {:else if loadError}
    <p role="alert">{loadError}</p>
  {:else}
    <form
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Field label="Название">
        {#snippet children({ id })}<input {id} bind:value={title} maxlength={80} />{/snippet}
      </Field>
      {#if titleError}<p role="alert">{titleError}</p>{/if}
      <Field label="Описание">
        {#snippet children({ id })}<textarea {id} bind:value={description} rows={2}></textarea>{/snippet}
      </Field>
      {#if descriptionError}<p role="alert">{descriptionError}</p>{/if}
      <Field label="Текст">
        {#snippet children({ id })}<textarea {id} bind:value={text} rows={8} class="mono"></textarea>{/snippet}
      </Field>
      <p>{counter.text}</p>
      {#if textError}<p role="alert">{textError}</p>{/if}
      {#if conflict}
        <p role="alert">Инструкции уже изменены (версия {conflict.current}). Обновите и повторите правку.</p>
        {#if draft !== null && draft !== serverText}
          <Disclosure title="Ваш вариант" open><p>{draft}</p></Disclosure>
        {/if}
        <Button onclick={() => void refresh()}>Обновить</Button>
      {/if}
      {#if canSave}
        <Button variant="primary" type="submit">Сохранить</Button>
      {:else}
        <Button variant="primary" type="submit" disabled reason={`Заполните название и текст до ${limit} байт`}>Сохранить</Button>
      {/if}
    </form>
  {/if}
</Sheet>

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }
  textarea {
    field-sizing: content;
    max-height: calc(var(--fs-input) * var(--lh-body) * 16);
  }
  /* field-sizing перекрывает rows: многоабзацный текст инструкции открывается полем в шесть строк, а не капсулой. */
  .mono {
    min-height: calc(var(--fs-input) * var(--lh-code) * 6 + var(--s10));
    font-family: var(--font-mono);
    line-height: var(--lh-code);
  }
  p {
    margin: 0;
    color: var(--muted);
    font-size: var(--fs-meta);
  }
  p[role='alert'] {
    color: var(--danger);
  }
</style>
