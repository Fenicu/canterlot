<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { canSendNow } from '#lib/api/gate.js';
  import { loadConfig } from '#lib/api/config.js';
  import { clock } from '#lib/clock.js';
  import AttachmentTray from '#lib/composer/AttachmentTray.svelte';
  import ContextIndicator from '#lib/composer/ContextIndicator.svelte';
  import InterruptButton from '#lib/composer/InterruptButton.svelte';
  import SendLater from '#lib/composer/SendLater.svelte';
  import { bindDraft } from '#lib/composer/draftBinding.js';
  import { ComposerModel, EMPTY_DRAFT_REASON, type Limits } from '#lib/composer/model.svelte.js';
  import { takeAttachmentNotice } from '#lib/composer/attachmentNotice.js';
  import { getEnterPref } from '#lib/composer/prefs.js';
  import { bindComposerRestore } from '#lib/composer/restore.js';
  import { bindReturned } from '#lib/composer/returned.js';
  import { decideEnter, isTouchPrimary, turnIsLive } from '#lib/composer/sendRules.js';
  import { getRuntime } from '#lib/app/runtime.js';
  import { sessionUploads } from '#lib/screens/chat/open.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { probe } from '#lib/perf/probe.js';

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
        serverNow: () => clock.serverNow()
      })
  );

  untrack(() => { model.errorMessage = takeAttachmentNotice(chat.id); });

  let node = $state<HTMLTextAreaElement | null>(null);
  let file = $state<HTMLInputElement | null>(null);
  let later = $state(false);
  let composing = $state(false);
  let held = false;
  let timer = 0;
  let tick = $state(0);
  let originX = 0;
  let originY = 0;
  const touch = isTouchPrimary();
  const reasonId = $props.id();
  const laterId = `${reasonId}-later`;

  const turn = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
  let armReset = false;
  const showInterrupt = $derived(turn || model.interruptLabel !== 'Прервать');
  const uploadsPending = $derived.by(() => {
    void tick;
    return uploads
      .list(chat.id)
      .some((item) => item.phase !== 'cancelled' && item.phase !== 'failed' && item.phase !== 'ready');
  });
  const notice = $derived.by(() => {
    void tick;
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

  $effect(() => {
    const status = chat.interrupt && typeof chat.interrupt.status === 'string' ? chat.interrupt.status : '';
    if (status) model.applyInterruptStatus(status);
  });

  $effect(() => {
    if (!turn) armReset = true;
    else if (armReset && !model.interruptRunning) {
      armReset = false;
      if (model.interruptLabel !== 'Прервать') model.interruptLabel = 'Прервать';
    }
  });

  onMount(() => {
    const stop = bindComposerRestore((text) => {
      model.text = text;
      if (node) node.value = text;
    });
    const stopReturned = bindReturned(chat.id, (returned) => model.takeBack(returned));
    void loadConfig()
      .then((cfg) => {
        limits = cfg.limits;
      })
      .catch(() => undefined);
    const pulse = setInterval(() => {
      tick += 1;
    }, 250);
    return () => {
      stop();
      stopReturned();
      clearInterval(pulse);
      window.clearTimeout(timer);
    };
  });

  function onKey(event: KeyboardEvent) {
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

  function picked() {
    if (file?.files?.length) model.attach(file.files);
    if (file) file.value = '';
  }

  function markSend(node: HTMLButtonElement) {
    $effect(() => {
      void tick;
      node.setAttribute('aria-disabled', model.gate.ok && !model.sending ? 'false' : 'true');
    });
  }

  function dropped(event: DragEvent) {
    const files = event.dataTransfer?.files;
    if (!files?.length) return;
    event.preventDefault();
    model.attach(files);
  }
</script>

<section class="composer" data-composer role="group" aria-label="Поле ввода" ondragover={(event) => event.preventDefault()} ondrop={dropped}>
  {#if notice}
    <p class="reason" id={reasonId}>{notice}</p>
  {:else}
    <p class="sr-only" id={reasonId}>{model.gate.ok ? '' : EMPTY_DRAFT_REASON}</p>
  {/if}
  <div class="capsule">
    <input bind:this={file} class="file" type="file" multiple tabindex="-1" aria-label="Выбрать файлы" onchange={picked} />
    <AttachmentTray {uploads} conversationId={chat.id} />
    <textarea
      bind:this={node}
      bind:value={model.text}
      use:bindDraft={{
        drafts: runtime.drafts,
        conversationId: chat.id,
        onRestore: (text) => {
          model.text = text;
        },
        onChange: (text) => {
          model.text = text;
        },
        onAttachments: (list) => {
          uploads.restore(chat.id, list);
          runtime.drafts.set(chat.id, { attachments: [] });
        }
      }}
      rows="1"
      enterkeyhint="enter"
      placeholder="Сообщение агенту…"
      aria-label="Сообщение"
      aria-describedby={reasonId}
      oncompositionstart={() => (composing = true)}
      oncompositionend={() => (composing = false)}
      onkeydown={onKey}
    ></textarea>
    <span class="attach"><IconButton icon="attach" label="Прикрепить файл" onclick={() => file?.click()} /></span>
    <ContextIndicator {chat} />
    <span class="extras"><InterruptButton show={showInterrupt} label={model.interruptLabel} onclick={() => model.interrupt()} /></span>
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
  </div>
  {#if !touch}<p class="hint">Enter: отправить · Shift+Enter: новая строка · Ctrl+Shift+Enter: отправить позже</p>{/if}
  <SendLater
    open={later}
    now={clock.serverNow()}
    blocked={uploadsPending}
    panelId={laterId}
    onclose={() => (later = false)}
    onpick={(at) => model.sendLater(at)}
  />
</section>

<style>
  .composer { display: flex; flex-direction: column; gap: var(--s2); padding: var(--s3) var(--gutter) max(var(--s4), env(safe-area-inset-bottom)); min-width: 0; position: relative; }
  .capsule {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto auto auto;
    grid-template-areas: 'input input input input input' 'attach context extras clock send';
    align-items: center;
    gap: var(--s2);
    min-width: 0;
    padding: var(--s1) var(--s3) var(--s3);
    border-radius: var(--radius-composer);
    background: var(--surface);
  }
  .capsule:has(:global(.tray)) {
    grid-template-areas: 'tray tray tray tray tray' 'input input input input input' 'attach context extras clock send';
  }
  .capsule:has(textarea:focus-visible) { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  textarea {
    grid-area: input;
    field-sizing: content;
    min-width: 0;
    width: 100%;
    min-height: var(--target);
    max-height: calc(var(--fs-input) * var(--lh-body) * 6 + var(--s5) * 2);
    padding: var(--s5);
    overflow: auto;
    resize: none;
    border: var(--s0);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--fs-input);
  }
  textarea:focus-visible { outline: none; }
  .attach { grid-area: attach; display: inline-flex; }
  .extras { grid-area: extras; display: inline-flex; min-width: 0; }
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
  .off { background: var(--raised); color: var(--muted); }
  .reason, .hint { margin: 0; padding-inline: var(--s5); color: var(--muted); font-size: var(--fs-sm); }
  .reason { color: var(--danger); }
</style>
