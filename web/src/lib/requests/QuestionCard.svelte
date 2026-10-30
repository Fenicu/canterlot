<script lang="ts">
  import type { RequestItem } from '#lib/stores/requests.svelte.js';
  import type { DecisionBody } from '#lib/api/client.js';
  import TruncatedNote from './TruncatedNote.svelte';

  type QuestionOption = {
    label: string;
    description?: string;
  };

  type QuestionDef = {
    id: string;
    text: string;
    header?: string;
    options?: Array<string | QuestionOption>;
    multi_select?: boolean;
    allow_custom?: boolean;
    is_other?: boolean;
    is_secret?: boolean;
  };

  let {
    request,
    onDecision,
    disabled = false
  }: {
    request: RequestItem;
    onDecision: (d: DecisionBody) => Promise<void>;
    disabled?: boolean;
  } = $props();

  const uid = $props.id();
  let formEl: HTMLFormElement | undefined = $state();
  let selectedSingle = $state<Record<string, string>>({});
  let selectedMulti = $state<Record<string, string[]>>({});
  let customValues = $state<Record<string, string>>({});

  $effect(() => {
    if (request.winnerDecision && typeof request.winnerDecision === 'object') {
      const winner = request.winnerDecision as Record<string, unknown>;
      const answers = (winner.answers ?? winner) as Record<string, unknown> | undefined;
      if (answers && typeof answers === 'object') {
        for (const [qid, ans] of Object.entries(answers)) {
          if (Array.isArray(ans)) {
            selectedMulti[qid] = ans.map(String);
          } else if (ans !== null && ans !== undefined) {
            selectedSingle[qid] = String(ans);
          }
        }
      }
    }
  });

  function normalizeQuestions(raw: unknown): QuestionDef[] {
    if (!raw || typeof raw !== 'object') return [];
    const rec = raw as Record<string, unknown>;
    const list = Array.isArray(rec.questions)
      ? rec.questions
      : rec.payload && typeof rec.payload === 'object' && Array.isArray((rec.payload as Record<string, unknown>).questions)
        ? (rec.payload as Record<string, unknown>).questions
        : null;

    if (Array.isArray(list)) {
      return list.slice(0, 4).map((q, idx) => {
        const qRec = q && typeof q === 'object' ? (q as Record<string, unknown>) : {};
        return {
          id: String(qRec.id ?? `q${idx}`),
          text: String(qRec.text ?? qRec.question ?? ''),
          header: qRec.header ? String(qRec.header) : undefined,
          options: Array.isArray(qRec.options) ? (qRec.options as Array<string | QuestionOption>) : undefined,
          multi_select: Boolean(qRec.multi_select ?? qRec.multiSelect),
          allow_custom: Boolean(qRec.allow_custom ?? qRec.is_other ?? qRec.allowCustom ?? qRec.isOther),
          is_other: Boolean(qRec.is_other ?? qRec.isOther),
          is_secret: Boolean(qRec.is_secret ?? qRec.isSecret)
        };
      });
    }

    if (rec.text || rec.question) {
      return [
        {
          id: String(rec.id ?? 'q0'),
          text: String(rec.text ?? rec.question ?? ''),
          header: rec.header ? String(rec.header) : undefined,
          options: Array.isArray(rec.options) ? (rec.options as Array<string | QuestionOption>) : undefined,
          multi_select: Boolean(rec.multi_select ?? rec.multiSelect),
          allow_custom: Boolean(rec.allow_custom ?? rec.is_other ?? rec.allowCustom ?? rec.isOther),
          is_other: Boolean(rec.is_other ?? rec.isOther),
          is_secret: Boolean(rec.is_secret ?? rec.isSecret)
        }
      ];
    }

    return [];
  }

  const questions = $derived(normalizeQuestions(request.payload));
  const isDisabled = $derived(disabled || request.state !== 'open');

  function answerOf(q: QuestionDef): string | string[] {
    const raw = customValues[q.id] ?? '';
    // Секрет уходит как введён: пробелы могут быть его частью.
    const custom = raw.trim() ? (q.is_secret ? raw : raw.trim()) : '';
    if (q.multi_select) {
      const picked = selectedMulti[q.id] ?? [];
      return custom && !picked.includes(custom) ? [...picked, custom] : [...picked];
    }
    return custom || selectedSingle[q.id] || '';
  }

  // Хаб отклоняет пустой ответ: «Ответить» доступна, только когда на каждый вопрос есть ответ.
  const complete = $derived(
    questions.length > 0 && questions.every((q) => answerOf(q).length > 0)
  );

  function disableIf(node: HTMLButtonElement, val: boolean) {
    node.disabled = val;
    return {
      update(v: boolean) {
        node.disabled = v;
      }
    };
  }

  function getOptionLabel(opt: string | QuestionOption): string {
    if (typeof opt === 'string') return opt;
    return opt.label ?? '';
  }

  function getOptionDesc(opt: string | QuestionOption): string | undefined {
    if (typeof opt === 'string') return undefined;
    return opt.description;
  }

  function toggleOption(q: QuestionDef, label: string) {
    if (isDisabled) return;
    if (q.multi_select) {
      const cur = selectedMulti[q.id] ?? [];
      if (cur.includes(label)) {
        selectedMulti[q.id] = cur.filter((x) => x !== label);
      } else {
        selectedMulti[q.id] = [...cur, label];
      }
    } else {
      selectedSingle[q.id] = label;
      customValues[q.id] = '';
    }
  }

  function bindCustomInput(node: HTMLInputElement, qId: string) {
    const onInput = () => {
      customValues[qId] = node.value;
      const q = questions.find((item) => item.id === qId);
      if (!q?.multi_select && node.value.trim()) {
        delete selectedSingle[qId];
      }
    };
    node.addEventListener('input', onInput);
    node.addEventListener('change', onInput);
    return {
      destroy() {
        node.removeEventListener('input', onInput);
        node.removeEventListener('change', onInput);
      }
    };
  }

  async function handleSubmit(e: MouseEvent | SubmitEvent) {
    e.preventDefault();
    if (isDisabled) return;

    if (formEl) {
      const inputs = formEl.querySelectorAll<HTMLInputElement>('input[data-custom="true"]');
      for (const input of inputs) {
        const sec = input.closest<HTMLElement>('[data-question]');
        const qId = sec?.getAttribute('data-question');
        if (qId && input.value) {
          customValues[qId] = input.value;
        }
      }
    }

    if (!complete) return;
    const answers: Record<string, string | string[]> = {};
    for (const q of questions) answers[q.id] = answerOf(q);
    await onDecision({
      kind: 'question',
      answers
    });
  }
</script>

<div class="question-card" data-disabled={isDisabled ? '' : undefined}>
  <div class="card-head">
    <span class="meta">Вопрос от агента</span>
  </div>

  <form bind:this={formEl} onsubmit={handleSubmit} class="questions-form">
    {#each questions as q, qIndex (q.id)}
      <section class="question-section" data-question={q.id}>
        {#if q.header}
          <span class="q-header">{q.header}</span>
        {/if}
        <h3 class="q-text" id={`${uid}-q${qIndex}`}>{q.text}</h3>

        <span class="selection-mode">
          {q.multi_select ? 'Выберите несколько вариантов' : 'Выберите один вариант'}
        </span>

        {#if q.options && q.options.length > 0}
          <div
            class="options-group"
            role={q.multi_select ? 'group' : 'radiogroup'}
            aria-label={q.header ? `${q.header}: ${q.text}` : q.text}
          >
            {#each q.options as opt, optIdx (typeof opt === 'string' ? opt : `${opt.label}-${optIdx}`)}
              {@const label = getOptionLabel(opt)}
              {@const desc = getOptionDesc(opt)}
              {@const isSelected = q.multi_select
                ? (selectedMulti[q.id] ?? []).includes(label)
                : selectedSingle[q.id] === label}
              <button
                type="button"
                class="option-item"
                role={q.multi_select ? 'checkbox' : 'radio'}
                aria-checked={isSelected}
                data-option={label}
                data-selected={isSelected ? '' : undefined}
                use:disableIf={isDisabled}
                onclick={() => toggleOption(q, label)}
              >
                <div class="opt-content">
                  <span class="opt-label">{label}</span>
                  {#if desc}
                    <span class="opt-desc">{desc}</span>
                  {/if}
                </div>
              </button>
            {/each}
          </div>
        {/if}

        {#if q.allow_custom || q.is_other}
          <div class="custom-input-wrap">
            <input
              type={q.is_secret ? 'password' : 'text'}
              class="custom-input"
              data-custom="true"
              placeholder="Свой ответ"
              aria-labelledby={`${uid}-q${qIndex}`}
              value={customValues[q.id] ?? ''}
              disabled={isDisabled}
              use:bindCustomInput={q.id}
            />
          </div>
        {/if}
      </section>
    {/each}

    <TruncatedNote payload={request.payload} />

    <div class="actions">
      <button
        type="submit"
        class="submit-btn"
        use:disableIf={isDisabled || !complete}
        onclick={handleSubmit}
      >
        Ответить
      </button>
    </div>
  </form>
</div>

<style>
  .question-card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    color: var(--text);
  }

  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .meta {
    font-size: var(--fs-meta);
    color: var(--muted);
  }

  .questions-form {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }

  .question-section {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }

  .q-header {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-normal);
    color: var(--secondary);
    font-weight: var(--weight-semibold);
  }

  .q-text {
    margin: var(--s0);
    font-size: var(--fs-title);
    font-weight: var(--weight-semibold);
    line-height: var(--lh-tight);
    color: var(--text);
  }

  .selection-mode {
    font-size: var(--fs-sm);
    color: var(--muted);
    margin-bottom: var(--s1);
  }

  .options-group {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }

  .option-item {
    display: flex;
    align-items: center;
    gap: var(--s6);
    padding: var(--s3) var(--s4);
    min-height: var(--target);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: background var(--duration-fast) var(--ease);
  }

  .option-item::before {
    content: '';
    flex-shrink: 0;
    width: var(--mark);
    height: var(--mark);
    margin: var(--s2);
    border-radius: var(--radius-round);
    outline: var(--focus-width) solid var(--muted);
    outline-offset: var(--s1);
  }

  .option-item:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .option-item:hover:not(:disabled) {
    background: var(--hover);
  }

  .option-item[data-selected],
  .option-item[data-selected]:hover:not(:disabled) {
    background: var(--selected);
  }

  .option-item[data-selected]::before {
    background: var(--accent);
    outline-color: var(--accent);
  }

  .option-item:disabled {
    opacity: var(--opacity-low);
    cursor: not-allowed;
  }

  .opt-content {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    min-width: var(--s0);
  }

  .opt-label {
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .opt-desc {
    font-size: var(--fs-sm);
    color: var(--secondary);
    overflow-wrap: anywhere;
  }

  .custom-input-wrap {
    margin-top: var(--s1);
  }

  .custom-input {
    width: 100%;
    box-sizing: border-box;
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text);
    font-size: var(--fs-body);
    font-family: var(--font-sans);
  }

  .custom-input::placeholder {
    color: var(--muted);
    opacity: 1;
  }

  .custom-input:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .custom-input:disabled {
    opacity: var(--opacity-low);
    cursor: not-allowed;
  }

  .actions {
    display: flex;
    margin-top: var(--s2);
  }

  .submit-btn {
    flex: 1;
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--accent);
    color: var(--accent-ink);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    cursor: pointer;
    transition: background var(--duration-fast) var(--ease), opacity var(--duration-fast) var(--ease);
  }

  .submit-btn:focus-visible {
    outline: var(--focus-width) solid var(--accent);
    outline-offset: var(--s1);
  }

  .submit-btn:disabled {
    opacity: var(--opacity-low);
    cursor: not-allowed;
  }

  .submit-btn:not(:disabled):hover {
    filter: brightness(1.1);
  }
</style>
