<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { canSendNow } from '$lib/api/gate';
  import { loadConfig } from '$lib/api/config';
  import { clock } from '$lib/clock';
  import AttachmentTray from '$lib/composer/AttachmentTray.svelte';
  import InterruptButton from '$lib/composer/InterruptButton.svelte';
  import SendLater from '$lib/composer/SendLater.svelte';
  import { bindDraft } from '$lib/composer/draftBinding';
  import { ComposerModel, type Limits } from '$lib/composer/model.svelte';
  import { getEnterPref } from '$lib/composer/prefs';
  import { bindComposerRestore } from '$lib/composer/restore';
  import { decideEnter, isTouchPrimary } from '$lib/composer/sendRules';
  import { getRuntime } from '$lib/app/runtime';
  import { sessionUploads } from '$lib/screens/chat/open';
  import type { ChatStore } from '$lib/stores/chat.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import IconButton from '$lib/ui/IconButton.svelte';
  import { probe } from '$lib/perf/probe';

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

  const activityState = $derived(
    chat.activity && typeof chat.activity === 'object' && 'state' in chat.activity
      ? String((chat.activity as { state?: unknown }).state ?? '')
      : ''
  );
  const slotRunning = $derived(
    typeof chat.queue.run_slot === 'object' && chat.queue.run_slot !== null && 'running' in chat.queue.run_slot
  );
  const turn = $derived((activityState === 'working' || activityState === 'stalled') && slotRunning);
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
    return gate.ok ? '' : gate.reason;
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
      node.setAttribute('aria-disabled', model.gate.ok ? 'false' : 'true');
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
  <AttachmentTray {uploads} conversationId={chat.id} />
  {#if notice}<p class="reason" id={reasonId}>{notice}</p>{/if}
  <div class="capsule">
    <input bind:this={file} class="file" type="file" multiple onchange={picked} />
    <IconButton icon="attach" label="Прикрепить файл" onclick={() => file?.click()} />
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
        }
      }}
      rows="2"
      enterkeyhint="enter"
      aria-label="Сообщение"
      aria-describedby={reasonId}
      oncompositionstart={() => (composing = true)}
      oncompositionend={() => (composing = false)}
      onkeydown={onKey}
    ></textarea>
    <InterruptButton show={showInterrupt} label={model.interruptLabel} onclick={() => model.interrupt()} />
    {#if !touch}
      <button
        type="button"
        class="clock"
        aria-label="Отправить позже"
        aria-haspopup="dialog"
        aria-expanded={later}
        aria-controls={laterId}
        onclick={() => (later = true)}
      >
        <Icon name="clock" />
      </button>
    {/if}
    <button
      type="button"
      class="send"
      class:off={!model.gate.ok}
      aria-label="Отправить"
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
  <p class="hint">
    {touch
      ? 'Enter: новая строка · отправка в очередь'
      : 'Enter: отправить · Shift+Enter: новая строка · Ctrl+Shift+Enter: отправить позже'}
  </p>
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
  .composer { display: flex; flex-direction: column; gap: var(--s2); padding: var(--s4); min-width: 0; position: relative; }
  .capsule {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--s2);
    min-width: 0;
    min-height: var(--composer-height);
    padding: var(--s2) var(--s3);
    border-radius: var(--radius-composer);
    background: var(--surface);
  }
  textarea {
    flex: 1 1 auto;
    min-width: 0;
    min-height: calc(var(--fs-input) * var(--lh-body) * 2);
    max-height: calc(var(--fs-input) * var(--lh-body) * 6);
    overflow: auto;
    resize: none;
    border: var(--s0);
    background: transparent;
    color: var(--text);
    font: inherit;
  }
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
  .clock { background: var(--raised); color: var(--text); }
  .off { background: var(--raised); color: var(--muted); }
  .reason, .hint { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
  .reason { color: var(--danger); }
</style>
