<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError } from '#lib/api/http.js';
  import { getDictationTerms, putDictationTerms } from '#lib/dictation/api.js';
  import { statusText } from '#lib/dictation/availability.js';
  import { dictationStatus } from '#lib/dictation/status.svelte.js';
  import {
    FREQUENT_TERMS,
    addTerm,
    missingFrequent,
    termErrorText,
    validateTerms,
    type DictationTerm
  } from '#lib/dictation/terms.js';
  import Button from '#lib/ui/Button.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import { dictationEnabled, saveDictationEnabled } from './preferences.js';

  let enabled = $state(dictationEnabled());
  let terms = $state<DictationTerm[]>([]);
  let saved = $state<string>('[]');
  let loadError = $state('');
  let error = $state('');
  let busy = $state(false);
  let frequentOpen = $state(false);
  let picked = $state<string[]>([]);

  const dirty = $derived(JSON.stringify(terms.map(clean)) !== saved);
  const frequent = $derived(missingFrequent(terms));
  const statusLine = $derived(statusText(dictationStatus.status));

  function clean(term: DictationTerm): DictationTerm {
    return { pattern: term.pattern.trim(), replacement: term.replacement.trim() };
  }

  function accept(next: DictationTerm[]): void {
    saved = JSON.stringify(next.map(clean));
    terms = next.map((term) => ({ ...term }));
  }

  async function loadTerms(): Promise<void> {
    loadError = '';
    try {
      accept((await getDictationTerms()).data.terms);
    } catch {
      loadError = 'Не удалось загрузить словарь замен. Повторите при восстановлении связи';
    }
  }

  onMount(() => {
    // Раздел открыт: причину недоступности показываем по свежему ответу.
    void dictationStatus.refresh(true);
    void loadTerms();
  });

  // Выбор хранится на устройстве, как остальные настройки ввода.
  $effect(() => {
    saveDictationEnabled(enabled);
  });

  function addRow(): void {
    error = '';
    terms = [...terms, { pattern: '', replacement: '' }];
  }

  function removeRow(index: number): void {
    error = '';
    terms = terms.filter((_, position) => position !== index);
  }

  function togglePicked(pattern: string): void {
    picked = picked.includes(pattern)
      ? picked.filter((item) => item !== pattern)
      : [...picked, pattern];
  }

  function addPicked(): void {
    error = '';
    let next = [...terms];
    for (const candidate of FREQUENT_TERMS) {
      if (!picked.includes(candidate.pattern)) continue;
      const result = addTerm(next, candidate);
      if (result.ok) next = result.terms;
    }
    terms = next;
    picked = [];
    frequentOpen = false;
  }

  async function save(): Promise<void> {
    if (busy) return;
    error = '';
    const code = validateTerms(terms.map(clean));
    if (code) {
      error = termErrorText(code) ?? 'Словарь заполнен неверно';
      return;
    }
    busy = true;
    try {
      accept((await putDictationTerms(terms.map(clean), crypto.randomUUID())).data.terms);
    } catch (err) {
      if (err instanceof ApiError) {
        error = termErrorText(err.code) ?? err.message;
      } else {
        error = 'Не удалось сохранить словарь. Повторите при восстановлении связи';
      }
    } finally {
      busy = false;
    }
  }
</script>

<section class="wrap" data-dictation-settings>
<h3 class="group-title">Диктовка</h3>
<div class="group">
  <section class="setting">
    <Toggle
      bind:checked={enabled}
      label="Кнопка микрофона"
      hint="В поле ввода: текст появляется во время речи"
      block
    />
  </section>
  <section class="setting">
    <h3>Служба распознавания</h3>
    <p class="note" data-dictation-status>{statusLine}</p>
  </section>
  <section class="setting">
    <h3>Словарь замен</h3>
    <p class="note">Распознанные слова заменяются перед вставкой. До 200 пар, шаблон и замена не длиннее 100 символов</p>
    {#if loadError}
      <p class="error" role="alert">{loadError}</p>
      <div class="actions"><Button onclick={loadTerms}>Повторить</Button></div>
    {:else}
      <ul class="terms">
        {#each terms as term, index (index)}
          <li data-dictation-term>
            <input
              class="field"
              data-term-pattern
              aria-label={`Шаблон, строка ${index + 1}`}
              placeholder="шаблон"
              value={term.pattern}
              oninput={(event) => (terms[index]!.pattern = (event.currentTarget as HTMLInputElement).value)}
            />
            <span class="arrow" aria-hidden="true">→</span>
            <input
              class="field"
              data-term-replacement
              aria-label={`Замена, строка ${index + 1}`}
              placeholder="замена"
              value={term.replacement}
              oninput={(event) => (terms[index]!.replacement = (event.currentTarget as HTMLInputElement).value)}
            />
            <button type="button" class="remove" aria-label={`Убрать строку ${index + 1}`} onclick={() => removeRow(index)}>Убрать</button>
          </li>
        {/each}
      </ul>
      {#if terms.length === 0}<p class="note">Пока пусто: добавьте пару или возьмите частые слова</p>{/if}
      <div class="actions">
        <Button onclick={addRow}>Добавить пару</Button>
        <span data-dictation-add-frequent>
          <Button variant="text" onclick={() => { frequentOpen = !frequentOpen; picked = []; }}>
            Добавить частые
          </Button>
        </span>
      </div>
      {#if frequentOpen}
        <div class="frequent" data-dictation-frequent-list>
          <p class="note">Отметьте пары: они встанут в словарь, сохраните список кнопкой ниже</p>
          <ul class="picks">
            {#each FREQUENT_TERMS as term (term.pattern)}
              <li>
                <label class="pick" class:present={!frequent.find((item) => item.term.pattern === term.pattern)?.missing}>
                  <input
                    type="checkbox"
                    checked={picked.includes(term.pattern)}
                    disabled={!frequent.find((item) => item.term.pattern === term.pattern)?.missing}
                    onchange={() => togglePicked(term.pattern)}
                  />
                  <span>{term.pattern} → {term.replacement}</span>
                </label>
              </li>
            {/each}
          </ul>
          <div class="actions">
            {#if picked.length === 0}
              <Button disabled reason="Ничего не отмечено">Добавить выбранные</Button>
            {:else}
              <Button onclick={addPicked}>Добавить выбранные</Button>
            {/if}
          </div>
        </div>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      <div class="actions">
        {#if dirty && !busy}
          <Button variant="primary" onclick={save}>Сохранить словарь</Button>
        {:else}
          <Button variant="primary" disabled reason={busy ? 'Сохраняю…' : 'Нет изменений'}>Сохранить словарь</Button>
        {/if}
      </div>
    {/if}
  </section>
</div>
</section>

<style>
  .wrap { margin-bottom: var(--s10); }
  .group-title { margin: var(--s10) var(--s6) var(--s4); color: var(--secondary); font-size: var(--fs-sm); font-weight: var(--weight-semibold); }
  .group { margin-bottom: var(--s6); padding: var(--s2) var(--s8); border-radius: var(--radius-card); background: var(--surface); }
  .setting { display: flex; flex-direction: column; gap: var(--s4); margin: var(--s0); padding: var(--s7) var(--s0); }
  .setting + .setting { border-top: var(--hairline) solid var(--line); }
  .setting h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .setting .note { margin: var(--s0); }
  .terms { display: flex; flex-direction: column; gap: var(--s3); list-style: none; margin: var(--s0); padding: var(--s0); }
  .terms li { display: grid; grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr) auto; align-items: center; gap: var(--s3); }
  .field { width: 100%; min-width: 0; min-height: var(--target); padding: var(--s5) var(--s8); border: var(--s0); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); font: inherit; font-size: var(--fs-meta); }
  .arrow { color: var(--muted); }
  .remove { min-height: var(--target); padding: var(--s4) var(--s5); border: var(--s0); border-radius: var(--radius-pill); background: var(--transparent); color: var(--secondary); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; justify-content: flex-start; gap: var(--s4); }
  .frequent { display: flex; flex-direction: column; gap: var(--s4); padding: var(--s5); border-radius: var(--radius); background: var(--raised); }
  .picks { display: grid; gap: var(--s2); list-style: none; margin: var(--s0); padding: var(--s0); }
  .pick { display: flex; align-items: center; gap: var(--s5); min-height: var(--target); color: var(--text); font-size: var(--fs-meta); }
  .pick input { width: var(--s8); height: var(--s8); accent-color: var(--accent); }
  .pick.present { color: var(--muted); }
  .error { color: var(--danger); margin: var(--s0); }
</style>
