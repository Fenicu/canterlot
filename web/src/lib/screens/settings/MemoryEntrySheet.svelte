<script lang="ts">
  import { untrack } from 'svelte';
  import { archiveMemory, createMemory, deleteMemory, getMemory, memoryRevisions, restoreMemory, updateMemory } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import Button from '#lib/ui/Button.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import {
    KINDS, actorLabel, conflictVersion, dateToValidUntil, entryStatus, kindLabel, levelLabel, secretClass, validUntilToDate,
    type MemoryDomain, type MemoryEntry, type MemoryRevision
  } from './memory.js';

  // `entry` — запись при открытии листа; `null` — новая. Дальше лист держит свою копию:
  // после сохранения она растёт версией, после конфликта подменяется актуальной.
  let { entry, domain, onclose, onchanged }: {
    entry: MemoryEntry | null;
    domain: MemoryDomain;
    onclose: () => void;
    /** Список за листом устарел: запись создана, изменена, перенесена или удалена. */
    onchanged: () => void;
  } = $props();

  let current = $state<MemoryEntry | null>(untrack(() => entry));
  let kind = $state(untrack(() => entry?.kind ?? 'reference'));
  let subject = $state(untrack(() => entry?.subject ?? ''));
  let body = $state(untrack(() => entry?.body ?? ''));
  let until = $state(untrack(() => validUntilToDate(entry?.valid_until)));
  let level = $state<'domain' | 'project'>(untrack(() => (entry?.level === 'project' ? 'project' : 'domain')));
  let project = $state('');
  let picked = $state<MemoryRevision | null>(null);

  let busy = $state(false);
  let error = $state('');
  let notice = $state('');
  /** Версия записи у хаба, если правка опоздала. */
  let conflict = $state<number | null>(null);
  /** Класс находки детектора секретов, пока владелец не подтвердил сохранение. */
  let secret = $state('');
  let confirmingDelete = $state(false);
  let revisions = $state<MemoryRevision[] | null>(null);
  let revisionsError = $state('');

  const status = $derived(current ? entryStatus(current) : null);
  const archived = $derived(current?.status === 'archived');
  const title = $derived(current ? 'Запись' : 'Новая запись');
  /** Запись переводят из домена в проект: каталог проекта нужен, пока записи его не назвали. */
  const needsProject = $derived(level === 'project' && (current === null || current.level !== 'project'));
  const canSave = $derived(!busy && subject.trim() !== '' && body.trim() !== '' && (!needsProject || project.trim() !== ''));

  function fill(next: MemoryEntry) {
    current = next; kind = next.kind; subject = next.subject; body = next.body; until = validUntilToDate(next.valid_until);
    level = next.level === 'project' ? 'project' : 'domain'; project = '';
  }
  function clearMessages() { error = ''; notice = ''; conflict = null; secret = ''; }

  async function run(action: () => Promise<void>, fallback: string) {
    if (busy) return;
    busy = true; clearMessages();
    try { await action(); }
    catch (err) {
      const version = conflictVersion(err);
      const cls = secretClass(err);
      if (version !== null) conflict = version;
      else if (cls !== null) secret = cls;
      else error = errorText(err, fallback);
    } finally { busy = false; }
  }

  function save(force = false) {
    void run(async () => {
      const key = crypto.randomUUID();
      const text = { kind, subject: subject.trim(), body, valid_until: dateToValidUntil(until), force_secret_warning: force || undefined };
      if (current) {
        const place = level !== current.level ? { level, project: level === 'project' ? project.trim() : undefined } : {};
        const next = (await updateMemory(current.id, { domain, expected_version: current.version, ...place, ...text }, key)).data;
        fill(next); notice = `Сохранено, версия ${next.version}`;
      } else {
        const created = (await createMemory({ domain, level, project: level === 'project' ? project.trim() : undefined, ...text }, key)).data;
        fill(created); notice = 'Запись создана';
      }
      onchanged();
    }, 'Не удалось сохранить запись. Повторите при восстановлении связи');
  }

  /** Подтверждённая правка: хаб отказал в проверке секретов, владелец решил сохранить. */
  const saveAnyway = () => save(true);

  function reload() {
    if (!current) return;
    const id = current.id;
    void run(async () => { fill((await getMemory(id, domain)).data); notice = 'Загружена актуальная версия'; onchanged(); }, 'Не удалось загрузить запись');
  }

  function archive() {
    const target = current;
    if (!target) return;
    void run(async () => { await archiveMemory(target.id, { domain, expected_version: target.version }, crypto.randomUUID()); onchanged(); onclose(); }, 'Не удалось перенести запись в архив');
  }

  function restore() {
    const target = current;
    if (!target) return;
    void run(async () => { await restoreMemory(target.id, { domain }, crypto.randomUUID()); onchanged(); onclose(); }, 'Не удалось вернуть запись из архива');
  }

  function remove() {
    const target = current;
    if (!target) return;
    void run(async () => { await deleteMemory(target.id, domain, crypto.randomUUID()); onchanged(); onclose(); }, 'Не удалось удалить запись');
  }

  /** Тело выбранной версии пишется как новая версия записи: история не теряется. */
  function restoreRevision() {
    const target = current; const old = picked;
    if (!target || !old) return;
    void run(async () => {
      const next = (await updateMemory(target.id, { domain, expected_version: target.version, body: old.body }, crypto.randomUUID())).data;
      fill(next); picked = null; revisions = null;
      notice = `Версия ${old.version} возвращена как версия ${next.version}`;
      onchanged();
      await loadRevisions();
    }, 'Не удалось вернуть версию. Повторите при восстановлении связи');
  }

  async function loadRevisions() {
    if (!current) return;
    revisionsError = '';
    try { revisions = (await memoryRevisions(current.id, domain)).data; }
    catch (err) { revisionsError = errorText(err, 'Не удалось загрузить прошлые версии'); }
  }
</script>

<Sheet open {title} subtitle={current ? `${kindLabel(current.kind)} · ${levelLabel(current)}` : undefined} {onclose}>
  <div class="entry" data-memory-sheet>
    {#if current && status}
      <p class="meta">
        <span class="status" data-tone={status.tone} data-role="memory-status">{status.word}</span>
        <span data-role="memory-version">Версия {current.version}</span>
        <span>Изменена {new Date(current.updated_at).toLocaleString('ru-RU')}</span>
      </p>
    {/if}
    {#if notice}<p class="notice" role="status" data-role="memory-notice">{notice}</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if conflict !== null}
      <div class="alert" role="alert" data-role="memory-conflict">
        <p>Запись изменена в другом месте, сейчас у неё версия {conflict}. Ваши правки не сохранены: загрузите актуальную версию и внесите их заново.</p>
        <Button variant="primary" onclick={reload}>Загрузить актуальную версию</Button>
      </div>
    {/if}
    {#if secret}
      <div class="alert" role="alert" data-role="memory-secret">
        <p>Текст похож на секрет (класс находки: {secret}). Память читают все сессии домена, пароли и ключи в ней не место. Если это не секрет, сохраните запись как есть.</p>
        <Button onclick={saveAnyway}>Сохранить всё равно</Button>
      </div>
    {/if}

    <form onsubmit={(event) => { event.preventDefault(); if (canSave) save(); }}>
      <Select label="Вид" options={KINDS} bind:value={kind} disabled={archived} />
      <Field label="Тема" hint="Одна строка до 160 символов">
        {#snippet children({ id, describedby, invalid })}
          <input {id} type="text" maxlength="160" bind:value={subject} disabled={archived} aria-describedby={describedby} aria-invalid={invalid} autocomplete="off" />
        {/snippet}
      </Field>
      <Textarea label="Текст" rows={6} bind:value={body} disabled={archived} />
      <Field label="Действует до" hint="Пусто: без срока. Истёкшие записи уходят в конец выдачи">
        {#snippet children({ id, describedby, invalid })}
          <input {id} type="date" bind:value={until} disabled={archived} aria-describedby={describedby} aria-invalid={invalid} />
        {/snippet}
      </Field>
      <Select label="Уровень" options={[{ value: 'domain', label: 'Весь домен' }, { value: 'project', label: 'Один проект' }]} bind:value={level} disabled={archived} />
      {#if needsProject}
        <Field label="Каталог проекта" hint="Канонический рабочий каталог проекта">
          {#snippet children({ id, describedby, invalid })}
            <input {id} type="text" bind:value={project} placeholder="/h/work/project" spellcheck="false" autocapitalize="off" aria-describedby={describedby} aria-invalid={invalid} />
          {/snippet}
        </Field>
      {/if}
      {#if !archived}
        <div class="actions">
          <Button variant="primary" type="submit" {...(canSave ? { disabled: false as const } : { disabled: true as const, reason: busy ? 'Сохраняю…' : 'Заполните тему и текст' })}>{current ? 'Сохранить' : 'Создать'}</Button>
        </div>
      {/if}
    </form>

    {#if current}
      <div class="actions secondary">
        {#if archived}
          <Button onclick={restore}>Вернуть из архива</Button>
        {:else}
          <Button onclick={archive}>В архив</Button>
        {/if}
        {#if !confirmingDelete}
          <button type="button" class="danger-link" onclick={() => confirmingDelete = true}>Удалить навсегда</button>
        {/if}
      </div>
      {#if confirmingDelete}
        <div class="confirm" role="group" aria-label="Удалить навсегда">
          <p>Запись и все её прошлые версии будут удалены без возможности вернуть. В журнале останется только факт удаления.</p>
          <div class="confirm-actions">
            <Button onclick={() => confirmingDelete = false}>Отмена</Button>
            <Button variant="primary" onclick={remove}>Удалить навсегда</Button>
          </div>
        </div>
      {/if}

      <details class="revisions" ontoggle={(event) => { if (event.currentTarget.open && revisions === null) void loadRevisions(); }}>
        <summary>Прошлые версии</summary>
        {#if revisionsError}<p class="error" role="alert">{revisionsError}</p>{/if}
        {#if revisions && revisions.length === 0}<p class="note">Прошлых версий нет</p>{/if}
        <ul data-role="memory-revisions">
          {#each revisions ?? [] as revision (revision.id)}
            <li>
              <button type="button" class="revision" aria-pressed={picked?.id === revision.id} onclick={() => picked = revision}>
                <strong>Версия {revision.version}</strong> <span class="note">{new Date(revision.at).toLocaleString('ru-RU')} · {actorLabel(revision.actor)}</span><span class="old">{revision.subject}</span>
              </button>
            </li>
          {/each}
        </ul>
        {#if picked}
          <div class="picked" data-role="memory-revision">
            <p class="note">Версия {picked.version}: {picked.subject}</p>
            <pre data-role="memory-revision-body">{picked.body}</pre>
            {#if !archived}<Button onclick={restoreRevision}>Вернуть эту версию</Button>{/if}
          </div>
        {/if}
      </details>
    {/if}
  </div>
</Sheet>

<style>
  .entry { display: grid; gap: var(--s6); }
  form { display: grid; gap: var(--s6); }
  .meta { display: flex; flex-wrap: wrap; gap: var(--s2) var(--s6); margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); }
  .status { font-weight: var(--weight-medium); color: var(--muted); }
  .status[data-tone='ok'] { color: var(--success); }
  .status[data-tone='warn'] { color: var(--warning); }
  .notice { margin: var(--s0); color: var(--success); font-size: var(--fs-sm); }
  .error { margin: var(--s0); color: var(--danger); }
  .alert { display: grid; gap: var(--s4); padding: var(--s5) var(--s6); border-radius: var(--radius-card); background: var(--raised); }
  .alert p { margin: var(--s0); color: var(--warning); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); }
  .secondary { padding-top: var(--s2); }
  .danger-link { min-height: var(--target); padding-inline: var(--s4); background: var(--transparent); color: var(--danger); font-size: var(--fs-sm); text-align: left; }
  .confirm { display: grid; gap: var(--s3); padding: var(--s5) var(--s6); border-radius: var(--radius-card); background: var(--raised); }
  .confirm p { margin: var(--s0); }
  .confirm-actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
  .revisions summary { min-height: var(--target); display: flex; align-items: center; color: var(--accent-link); cursor: pointer; }
  .revisions ul { display: grid; gap: var(--s3); margin: var(--s0); padding: var(--s0); list-style: none; }
  .revision { display: flex; flex-wrap: wrap; gap: var(--s2) var(--s4); width: 100%; min-height: var(--target); padding: var(--s3) var(--s4); border: 0; border-radius: var(--radius-sm); background: var(--transparent); color: inherit; text-align: left; }
  .revision[aria-pressed='true'] { background: var(--selected); }
  .picked { display: grid; gap: var(--s4); margin-top: var(--s4); padding: var(--s5) var(--s6); border-radius: var(--radius-card); background: var(--raised); }
  .picked pre { margin: var(--s0); font: inherit; white-space: pre-wrap; overflow-wrap: anywhere; }
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .old { flex-basis: 100%; overflow-wrap: anywhere; color: var(--secondary); font-size: var(--fs-sm); }
</style>
