<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import { canSendNow } from '#lib/api/gate.js';
  import { loadConfig } from '#lib/api/config.js';
  import { getAttachment, putConversationDraft } from '#lib/api/client.js';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import AttachmentTray from '#lib/composer/AttachmentTray.svelte';
  import ContextIndicator from '#lib/composer/ContextIndicator.svelte';
  import DictationPanel from '#lib/composer/DictationPanel.svelte';
  import { filesFromPayload, hasFileDrag } from '#lib/composer/intake.js';
  import { ComposerModel, EMPTY_DRAFT_REASON, type Limits } from '#lib/composer/model.svelte.js';
  import { takeAttachmentNotice } from '#lib/composer/attachmentNotice.js';
  import { getEnterPref } from '#lib/composer/prefs.js';
  import { insertDictated, type DraftAnchor } from '#lib/dictation/insertion.js';
  import { createDictationSession } from '#lib/dictation/live.js';
  import type { DictationSession, DictationView } from '#lib/dictation/session.js';
  import { dictationStatus } from '#lib/dictation/status.svelte.js';
  import { dictationEnabled } from '#lib/screens/settings/preferences.js';
  import { bindComposerRestore } from '#lib/composer/restore.js';
  import { bindReturned } from '#lib/composer/returned.js';
  import { decideEnter, isTouchPrimary } from '#lib/composer/sendRules.js';
  import { acceptsWithTab, visibleSuggestion } from '#lib/composer/suggestion.js';
  import { getRuntime } from '#lib/app/runtime.js';
  import { sessionUploads } from '#lib/screens/chat/open.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { probe } from '#lib/perf/probe.js';
  import type { Component } from 'svelte';

  let { chat }: { chat: ChatStore } = $props();
  const runtime = getRuntime();
  const uploads = sessionUploads();
  let limits = $state<Limits>({
    max_input_text_bytes: 262_144,
    queue_limit: 100,
    max_attachment_bytes: 104_857_600,
    max_attachments_per_input: 10
  });
  const model = untrack(
    () =>
      new ComposerModel({
        conversationId: chat.id,
        chat,
        ops: runtime.journal,
        drafts: runtime.drafts,
        uploads,
        config: () => limits,
        canSend: () => canSendNow(),
        connection: () => runtime.connection.status,
        probe,
        serverNow: () => clock.serverNow(),
        deviceId: runtime.deviceId,
        serverDrafts: {
          put: async (text, attachmentIds, baseRevision) => {
            const response = await putConversationDraft(chat.id, {
              text,
              attachment_ids: attachmentIds,
              base_revision: baseRevision,
              device_id: runtime.deviceId
            });
            return response.data;
          }
        },
        attachmentInfo: async (id) => {
          const response = await getAttachment(id);
          return {
            id: response.data.id,
            name: response.data.name,
            size: response.data.size
          };
        },
        online: () => navigator.onLine
      })
  );

  untrack(() => { model.errorMessage = takeAttachmentNotice(chat.id); });

  let node = $state<HTMLTextAreaElement | null>(null);
  let file = $state<HTMLInputElement | null>(null);
  let later = $state(false);
  // Окно «Отправить позже» открывают нечасто: чанк едет только вместе с ним.
  let SendLater = $state<Component<{ open: boolean; now: number; blocked?: boolean; panelId: string; onclose: () => void; onpick: (at: number) => void }> | null>(null);

  $effect(() => {
    if (later && !SendLater) import('#lib/composer/SendLater.svelte').then((m) => { SendLater = m.default; });
  });
  let composing = $state(false);
  let receiving = $state(false);
  let dictation = $state<DictationSession | null>(null);
  let dictationView = $state<DictationView | null>(null);
  let dictationAnchor = $state<DraftAnchor | null>(null);
  let dictationNotice = $state('');
  // Кнопка микрофона: служба доступна, выбор владельца включён, микрофон ещё не пропадал.
  const canDictate = $derived(
    !dictationView &&
      dictationEnabled() &&
      dictationStatus.available &&
      !dictationStatus.missing &&
      typeof navigator !== 'undefined' &&
      Boolean(navigator.mediaDevices?.getUserMedia)
  );
  const composerBusy = $derived(model.sending);
  let dropDepth = 0;
  let held = false;
  let timer = 0;
  let originX = 0;
  let originY = 0;
  const touch = isTouchPrimary();
  const reasonId = $props.id();
  const laterId = `${reasonId}-later`;
  const suggestionId = `${reasonId}-suggestion`;

  const uploadsPending = $derived.by(() => {
    void ticker.now;
    return uploads
      .list(chat.id)
      .some((item) => item.phase !== 'cancelled' && item.phase !== 'failed' && item.phase !== 'ready');
  });
  // Подсказка живёт только в placeholder: в черновик и в отправку она не попадает.
  const suggestion = $derived.by(() => {
    void ticker.now;
    const tray = uploads.list(chat.id).filter((item) => item.phase !== 'cancelled' && item.phase !== 'failed');
    const slot = chat.queue.run_slot;
    return visibleSuggestion({
      suggestion: chat.promptSuggestion,
      text: model.text,
      attachments: tray.length,
      uploadsPending,
      harness: (chat.detail?.harness as { id?: string } | undefined)?.id,
      activity: chat.activity?.state,
      runSlotFree: !slot || slot === 'free'
    });
  });
  const notice = $derived.by(() => {
    void ticker.now;
    if (model.errorMessage) return model.errorMessage;
    const gate = model.gate;
    return gate.ok || gate.reason === EMPTY_DRAFT_REASON ? '' : gate.reason;
  });
  const fitsContent = typeof CSS !== 'undefined' && CSS.supports('field-sizing', 'content');

  $effect(() => {
    void model.text;
    if (fitsContent || !node) return;
    node.style.height = 'auto';
    node.style.height = `${node.scrollHeight}px`;
  });

  let readySignature = '';

  onMount(() => {
    const stop = bindComposerRestore((text) => {
      model.text = text;
      if (node) node.value = text;
      model.typed();
    });
    const stopReturned = bindReturned(chat.id, (returned) => model.takeBack(returned));
    // Файл, брошенный мимо композера, не должен уводить приложение на этот файл.
    const blockDrop = (event: DragEvent) => {
      if (hasFileDrag(event.dataTransfer)) event.preventDefault();
    };
    const endDrop = () => {
      dropDepth = 0;
      receiving = false;
    };
    window.addEventListener('dragover', blockDrop);
    window.addEventListener('drop', blockDrop);
    window.addEventListener('dragend', endDrop);
    void loadConfig()
      .then((cfg) => {
        limits = cfg.limits;
      })
      .catch(() => undefined);
    const releaseTicker = ticker.retain();
    void dictationStatus.refresh();
    // Уход в фон заканчивает запись через обычный путь «Готово»: итог не теряется.
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') {
        void model.flushDraft();
        dictation?.finish();
      } else {
        void dictationStatus.refresh();
      }
    };
    document.addEventListener('visibilitychange', onVisibility);
    void model.startDraft(chat.draft ?? null).then(() => model.remoteDraft(chat.draft));
    return () => {
      stop();
      dictation?.dispose();
      dictation = null;
      dictationView = null;
      stopReturned();
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('dragover', blockDrop);
      window.removeEventListener('drop', blockDrop);
      window.removeEventListener('dragend', endDrop);
      releaseTicker();
      window.clearTimeout(timer);
      void model.leaveDraft();
    };
  });

  $effect(() => {
    void chat.draftSerial;
    if (chat.draftSerial === 0) return;
    model.remoteDraft(chat.draft);
  });

  $effect(() => {
    if (model.draftApplied === 0 || !node) return;
    if (document.activeElement === node) {
      const end = node.value.length;
      node.setSelectionRange(end, end);
    }
  });

  $effect(() => {
    if (runtime.connection.status.phase === 'live') void model.flushDraft();
  });

  $effect(() => {
    void ticker.now;
    const signature = uploads
      .list(chat.id)
      .filter((item) => item.phase === 'ready' && item.attachmentId)
      .map((item) => item.attachmentId)
      .join('|');
    if (signature === readySignature) return;
    readySignature = signature;
    model.attachmentsChanged();
  });

  function acceptSuggestion() {
    const text = suggestion;
    if (!text) return;
    model.text = text;
    if (node) {
      node.value = text;
      node.focus();
      node.setSelectionRange(text.length, text.length);
    }
    model.typed();
  }

  function onKey(event: KeyboardEvent) {
    if (suggestion && !touch && acceptsWithTab({
      key: event.key,
      shiftKey: event.shiftKey,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      altKey: event.altKey,
      isComposing: event.isComposing || composing
    })) {
      event.preventDefault();
      acceptSuggestion();
      return;
    }
    const decision = decideEnter({
      key: event.key,
      shiftKey: event.shiftKey,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      altKey: event.altKey,
      isComposing: event.isComposing || composing,
      keyCode: event.keyCode,
      pointer: isTouchPrimary() ? 'touch' : 'mouse',
      pref: getEnterPref()
    });
    if (decision === 'newline' || decision === 'ignore') return;
    event.preventDefault();
    if (decision === 'send_later_menu') later = true;
    else void model.send();
  }

  function pointerDown(event: PointerEvent) {
    event.preventDefault();
    held = false;
    originX = event.clientX;
    originY = event.clientY;
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      held = true;
      later = true;
    }, 500);
  }

  function pointerMove(event: PointerEvent) {
    if (Math.hypot(event.clientX - originX, event.clientY - originY) > 8) window.clearTimeout(timer);
  }

  function pointerUp() {
    window.clearTimeout(timer);
  }

  function sendClick() {
    if (held) {
      held = false;
      return;
    }
    void model.send();
  }

  function startDictation() {
    if (!canDictate || dictation) return;
    const start = node?.selectionStart ?? model.text.length;
    const end = node?.selectionEnd ?? model.text.length;
    dictationAnchor = { text: model.text, start, end };
    dictationNotice = '';
    const session = createDictationSession({
      onview: (view) => {
        dictationView = view;
        if (view.mic === 'missing') dictationStatus.hideForMissingMicrophone();
        if (view.phase === 'closed') {
          dictation = null;
          dictationView = null;
        }
      },
      oninsert: (text, notice) => void insertDictation(text, notice),
      onunavailable: () => dictationStatus.noteUnavailable()
    });
    dictation = session;
    void session.start();
  }

  // Вставка ровно одна: её решает сессия, а черновик сохраняется обычным путём поля ввода.
  async function insertDictation(text: string, notice: string) {
    const anchor = dictationAnchor ?? { text: model.text, start: model.text.length, end: model.text.length };
    const result = insertDictated(model.text, anchor, text);
    dictationAnchor = null;
    model.text = result.text;
    model.typed();
    void model.flushDraft();
    if (notice) dictationNotice = notice;
    // Поле ввода возвращается на место после панели: курсор ставим, когда оно уже смонтировано.
    await tick();
    if (!node) return;
    node.value = result.text;
    node.focus();
    node.setSelectionRange(result.caret, result.caret);
  }

  function cancelDictation() {
    dictation?.cancel();
    dictation = null;
    dictationView = null;
    dictationAnchor = null;
  }

  function picked() {
    if (file?.files?.length) model.attach(file.files);
    if (file) file.value = '';
  }

  function pasted(event: ClipboardEvent) {
    const files = filesFromPayload(event.clipboardData);
    if (files.length) model.attach(files);
  }

  function dragEnter(event: DragEvent) {
    if (!hasFileDrag(event.dataTransfer)) return;
    event.preventDefault();
    dropDepth += 1;
    receiving = true;
  }

  function dragOver(event: DragEvent) {
    if (!hasFileDrag(event.dataTransfer)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
  }

  function dragLeave() {
    if (dropDepth === 0) return;
    dropDepth -= 1;
    if (dropDepth === 0) receiving = false;
  }

  function markSend(node: HTMLButtonElement) {
    $effect(() => {
      void ticker.now;
      node.setAttribute('aria-disabled', model.gate.ok && !model.sending ? 'false' : 'true');
    });
  }

  // Клавиатура и рост вложений меняют высоту поля: курсор в конце текста остаётся виден.
  function keepCaret(el: HTMLTextAreaElement) {
    let frame = 0;
    const atEnd = () => document.activeElement === el && (el.selectionEnd ?? 0) >= el.value.length;
    const settle = () => {
      frame = 0;
      if (atEnd()) el.scrollTop = el.scrollHeight;
    };
    const reveal = () => {
      frame = 0;
      if (!atEnd()) return;
      // Поле сначала меняет высоту, и только следующим кадром появляется новый предел прокрутки.
      el.scrollTop = el.scrollHeight;
      frame = requestAnimationFrame(settle);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(reveal);
    };
    el.addEventListener('focus', schedule);
    el.addEventListener('input', schedule);
    el.addEventListener('keyup', schedule);
    window.addEventListener('resize', schedule);
    const observer = new ResizeObserver(schedule);
    observer.observe(el);
    const view = window.visualViewport;
    view?.addEventListener('resize', schedule);
    view?.addEventListener('scroll', schedule);
    return {
      destroy() {
        if (frame) cancelAnimationFrame(frame);
        observer.disconnect();
        el.removeEventListener('focus', schedule);
        el.removeEventListener('input', schedule);
        el.removeEventListener('keyup', schedule);
        window.removeEventListener('resize', schedule);
        view?.removeEventListener('resize', schedule);
        view?.removeEventListener('scroll', schedule);
      }
    };
  }

  function dropped(event: DragEvent) {
    const files = filesFromPayload(event.dataTransfer);
    dropDepth = 0;
    receiving = false;
    if (!files.length) return;
    event.preventDefault();
    model.attach(files);
  }
</script>

<section
  class="composer"
  data-composer
  role="group"
  aria-label="Поле ввода"
  ondragenter={dragEnter}
  ondragover={dragOver}
  ondragleave={dragLeave}
  ondrop={dropped}
>
  {#if notice && !dictationView}
    <p class="reason" id={reasonId}>{notice}</p>
  {:else}
    <p class="sr-only" id={reasonId}>{model.gate.ok ? '' : EMPTY_DRAFT_REASON}</p>
  {/if}
  <div class="capsule" class:receiving class:dictating={dictationView}>
    {#if dictationView}
      <DictationPanel
        view={dictationView}
        onfinish={() => dictation?.finish()}
        oncancel={cancelDictation}
        onpartial={() => dictation?.insertPartial()}
      />
    {:else}
      {#if receiving}<p class="dropzone" aria-hidden="true">Отпустите файлы</p>{/if}
      <input bind:this={file} class="file" type="file" multiple tabindex="-1" aria-label="Выбрать файлы" onchange={picked} />
      <AttachmentTray {uploads} conversationId={chat.id} />
      <textarea
        bind:this={node}
        bind:value={model.text}
        use:keepCaret
        oninput={() => model.typed()}
        onblur={() => void model.flushDraft()}
        rows="1"
        enterkeyhint="enter"
        placeholder={suggestion ?? 'Сообщение агенту…'}
        aria-label="Сообщение"
        aria-describedby={suggestion ? `${reasonId} ${suggestionId}` : reasonId}
        oncompositionstart={() => (composing = true)}
        oncompositionend={() => (composing = false)}
        onkeydown={onKey}
        onpaste={pasted}
      ></textarea>
      <span class="attach"><IconButton icon="attach" label="Прикрепить файл" onclick={() => file?.click()} /></span>
      {#if canDictate}
        <span class="mic"><IconButton icon="mic" label="Диктовка" inert={composerBusy} onclick={startDictation} /></span>
      {/if}
      <ContextIndicator {chat} />
      {#if suggestion}
        <p class="sr-only" id={suggestionId}>Подсказка: {suggestion}. {touch ? 'Кнопка «Подставить» вставит её в поле' : 'Tab вставит её в поле'}</p>
      {/if}
      {#if touch && suggestion}
        <button type="button" class="accept" data-suggestion-accept onmousedown={(event) => event.preventDefault()} onclick={acceptSuggestion}>Подставить</button>
      {/if}
      {#if !touch}
        <button
          type="button"
          class="clock"
          aria-label="Отправить позже"
          aria-haspopup="dialog"
          aria-expanded={later}
          aria-controls={laterId}
          onclick={() => (later = !later)}
        >
          <Icon name="clock" />
        </button>
      {/if}
      <button
        type="button"
        class="send"
        class:off={!model.gate.ok || model.sending}
        aria-label="Отправить"
        aria-busy={model.sending}
        use:markSend
        onpointerdown={pointerDown}
        onpointermove={pointerMove}
        onpointerup={pointerUp}
        onmousedown={(event) => event.preventDefault()}
        oncontextmenu={(event) => {
          event.preventDefault();
          later = true;
        }}
        onclick={sendClick}
      >
        <Icon name="send" />
      </button>
      {/if}
  </div>
  {#if dictationNotice}<p class="notice" role="status" data-dictation-notice>{dictationNotice}</p>{/if}
  {#if !touch && !dictationView}<p class="hint">{suggestion ? 'Tab: подставить подсказку · ' : ''}Enter: отправить · Shift+Enter: новая строка · Ctrl+Shift+Enter: отправить позже</p>{/if}
  {#if SendLater}
    <SendLater
      open={later}
      now={clock.serverNow()}
      blocked={uploadsPending}
      panelId={laterId}
      onclose={() => (later = false)}
      onpick={(at) => model.sendLater(at)}
    />
  {/if}
</section>

<style>
  .composer { display: flex; flex-direction: column; gap: var(--s2); max-height: var(--composer-limit); padding: var(--s3) var(--gutter) max(var(--s4), env(safe-area-inset-bottom)); min-width: 0; position: relative; }
  .capsule {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto auto;
    grid-template-areas: 'input input input input input' 'attach mic context clock send';
    align-items: center;
    gap: var(--s2);
    min-width: 0;
    min-height: 0;
    padding: var(--s1) var(--s3) var(--s3);
    border-radius: var(--radius-composer);
    background: var(--surface);
    position: relative;
  }
  .capsule.receiving { outline: var(--focus-width) dashed var(--accent); outline-offset: var(--s1); background: var(--accent-soft); }
  .dropzone { position: absolute; inset: var(--s1); z-index: 1; display: flex; align-items: center; justify-content: center; margin: 0; border-radius: var(--radius-card); background: var(--accent-soft); color: var(--text); font-size: var(--fs-meta); pointer-events: none; }
  .capsule:has(:global(.tray)) {
    grid-template-areas: 'tray tray tray tray tray' 'input input input input input' 'attach mic context clock send';
  }
  .capsule:has(textarea:focus-visible) { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  textarea {
    grid-area: input;
    field-sizing: content;
    min-width: 0;
    width: 100%;
    min-height: var(--target);
    max-height: min(calc(var(--fs-input) * var(--lh-body) * 6 + var(--s5) * 2), calc(var(--composer-limit) * .5));
    padding: var(--s5);
    overflow: auto;
    resize: none;
    border: var(--s0);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--fs-input);
  }
  /* Клавиатура забрала высоту: поле показывает три строки и прокручивается внутри. */
  :global(html[data-compact]) textarea {
    max-height: calc(var(--fs-input) * var(--lh-body) * 3 + var(--s5) * 2);
  }
  textarea:focus-visible { outline: none; }
  .attach { grid-area: attach; display: inline-flex; }
  .mic { grid-area: mic; display: inline-flex; }
  .file { position: absolute; width: var(--s1); height: var(--s1); overflow: hidden; clip-path: inset(50%); }
  .clock, .send {
    width: var(--target);
    height: var(--target);
    min-width: var(--target);
    min-height: var(--target);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: var(--s0);
    border-radius: var(--radius-round);
    background: var(--accent);
    color: var(--accent-ink);
  }
  .clock { grid-area: clock; background: var(--transparent); color: var(--text); }
  .send { grid-area: send; }
  .accept { grid-area: clock; min-height: var(--target); padding-inline: var(--s5); border: var(--s0); border-radius: var(--radius-round); background: var(--raised); color: var(--text); font-size: var(--fs-meta); }
  .off { background: var(--raised); color: var(--muted); }
  .reason, .hint, .notice { margin: 0; padding-inline: var(--s5); color: var(--muted); font-size: var(--fs-sm); }
  .reason { color: var(--danger); }
  .capsule.dictating { grid-template-areas: 'dictation'; grid-template-columns: minmax(0, 1fr); padding: var(--s1); }
</style>
