<script lang="ts">
  import { ApiError } from '#lib/api/http.js';
  import { deleteConversation, getConversation, patchConversation, setConversationGroups, patchSettings, setCrunch } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { intents } from '#lib/ops/intent.js';
  import { stopSession } from '#lib/screens/chat/controls.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import { PERMISSION_MODES, effortLabel, permissionHint, permissionLabel, permissionValue } from '#lib/format/labels.js';
  import { onMount, untrack } from 'svelte';
  import Button from '#lib/ui/Button.svelte';
  import ChipToggle from '#lib/ui/ChipToggle.svelte';
  import ColorSwatches from '#lib/ui/ColorSwatches.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Disclosure from '#lib/ui/Disclosure.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import { dismissable, type DismissReason } from '#lib/ui/dismiss.js';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import type { MachineRecord } from '#lib/stores/machines.svelte.js';
  import { turnRunning, turnText, type Activity } from './activityText';
  import { COLOR_KEYS, COLOR_NAMES } from './colors';
  import { menuTop } from './menuPlace';

  let {
    item,
    groups,
    machine = null,
    touch = isTouchPrimary(),
    anchor = 0,
    anchorTop = 0,
    ongroups,
    onclose,
    ondeleted
  }: {
    item: ListRow;
    groups: { id: string; name: string }[];
    machine?: MachineRecord | null;
    touch?: boolean;
    anchor?: number;
    anchorTop?: number;
    ongroups?: () => void;
    onclose: () => void;
    ondeleted?: (id: string) => void;
  } = $props();

  const STOP_POLLS = 60;
  const STOP_POLL_MS = 500;
  const seed = untrack(() => item);
  let title = $state(seed.title);
  let pinned = $state(seed.pinned);
  let chosen = $state(seed.color ?? 'neutral');
  let members = $state(seed.groups.slice());
  let model = $state(seed.model ?? 'opus');
  let effort = $state(seed.effort ?? 'medium');
  let permission = $state(permissionValue(seed.permission_mode));
  let crunch = $state(seed.crunch?.enabled ?? false);
  let resumeText = $state('Лимиты восстановлены, продолжай с того места, где остановился.');
  let reasons = $state<string[]>([]);
  let confirmingDelete = $state(false);
  let stopFirst = $state(false);
  let stopping = $state(false);
  let deleteError = $state('');
  // Ключи живут, пока сессия не удалена: повторное нажатие после обрыва повторяет те же операции.
  const deleteKey = crypto.randomUUID();
  const stopKey = crypto.randomUUID();
  let sentTitle = seed.title;
  let sentPin = seed.pinned;
  let sentCrunch = seed.crunch?.enabled ?? false;
  // В строке списка текста продолжения нет, а хаб заменил бы сохранённый текст присланным.
  const seeded = getConversation(seed.id)
    .then((detail) => {
      if (detail.data.crunch.resume_text) resumeText = detail.data.crunch.resume_text;
    })
    .catch(() => undefined);

  let saveError = $state('');

  /** Правка уходит сразу; отказ или обрыв возвращают прежнее значение и показывают причину. */
  async function attempt(fallback: string, call: () => Promise<unknown>, revert: () => void): Promise<boolean> {
    saveError = '';
    try {
      await call();
      return true;
    } catch (err) {
      revert();
      saveError = errorText(err, fallback);
      return false;
    }
  }

  $effect(() => {
    const value = pinned;
    if (value === sentPin) return;
    sentPin = value;
    void attempt('Не удалось закрепить', () => patch({ pinned: value }), () => {
      sentPin = !value;
      pinned = !value;
    });
  });
  $effect(() => {
    const value = crunch;
    if (value === sentCrunch) return;
    sentCrunch = value;
    void attempt('Не удалось сохранить кранч', saveCrunchNow, () => {
      sentCrunch = !value;
      crunch = !value;
    });
  });
  const harness = $derived(machine?.harnesses.find((entry) => entry.id === item.harness.id));
  const known = (values: string[], current: string) => (values.includes(current) ? values : [current, ...values]);
  const modelOptions = $derived(known(harness?.models.map((entry) => entry.id) ?? [], model).map((value) => ({ value, label: harness?.models.find((entry) => entry.id === value)?.title || value })));
  const effortOptions = $derived(known(harness?.models.find((entry) => entry.id === model)?.efforts ?? ['low', 'medium', 'high'], effort).map((value) => ({ value, label: effortLabel(value) })));
  const permissionOptions = $derived(known(harness?.permission_modes ?? [...PERMISSION_MODES], permission).map((value) => ({ value, label: permissionLabel(value) })));
  const swatches = COLOR_KEYS.map((key) => ({ key, name: COLOR_NAMES[key] }));

  let panel = $state<HTMLElement | null>(null);
  let content = $state<HTMLElement | null>(null);
  let top = $state(untrack(() => anchor));
  let room = $state<number | null>(null);
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  function place() {
    const parent = panel?.offsetParent;
    if (!panel || !content || !(parent instanceof HTMLElement)) return;
    const height = parent.clientHeight;
    const style = getComputedStyle(panel);
    const full = content.offsetHeight + parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
    top = menuTop({ full, height, rowTop: anchorTop, rowBottom: anchor });
    room = height - top;
  }

  onMount(() => {
    place();
    if (!panel || !content || typeof ResizeObserver === 'undefined') return;
    // Предупреждение архива и настройки меняют высоту: окно целиком остаётся на экране.
    const observer = new ResizeObserver(() => place());
    observer.observe(content);
    return () => observer.disconnect();
  });

  function dismiss() {
    void saveTitle();
    onclose();
    opener?.focus();
  }

  function dismissBy(reason: DismissReason) {
    if (reason === 'escape') return dismiss();
    // Нажатие снаружи уже забрало фокус себе, и поле может исчезнуть раньше своего blur.
    void saveTitle();
    onclose();
  }

  const ownToggle = (target: Element) => target.closest(`[data-session="${CSS.escape(item.id)}"] button`) !== null;

  const reasonText: Record<string, string> = {
    crunch: 'кранч',
    scheduled: 'отложенные сообщения'
  };

  async function reasonLabels(list: string[]): Promise<string[]> {
    const turn = list.includes('live_execution') ? (await sessionNow(item.id).catch(() => null))?.turn : '';
    return list.map((reason) => (reason === 'live_execution' ? turn || 'идёт ход агента' : (reasonText[reason] ?? 'активная работа')));
  }

  async function patch(body: { title?: string; color?: string; pinned?: boolean; archived?: boolean; force?: boolean }) {
    const id = item.id;
    await intents.run(`conversation:${id}:${JSON.stringify(body)}`, (key) => patchConversation(id, body, key));
  }

  async function saveTitle() {
    const next = title.trim();
    if (next === item.title || next === sentTitle) return;
    const before = sentTitle;
    sentTitle = next;
    await attempt('Не удалось переименовать', () => patch({ title: next }), () => {
      sentTitle = before;
      title = before;
    });
  }

  async function saveColor(value: string) {
    const before = chosen;
    chosen = value;
    await attempt('Не удалось сменить цвет', () => patch({ color: value }), () => {
      chosen = before;
    });
  }

  async function saveSetting(body: { model?: string; effort?: string; permission_mode?: string }, revert: () => void) {
    const id = item.id;
    await attempt(
      'Не удалось сохранить настройку',
      () => intents.run(`settings:${id}:${JSON.stringify(body)}`, (key) => patchSettings(id, body, key)),
      revert
    );
  }

  // Select уже записал новое значение через bind: прежнее помним по последнему сохранённому.
  let savedModel = seed.model ?? 'opus';
  let savedEffort = seed.effort ?? 'medium';
  let savedPermission = permissionValue(seed.permission_mode);

  async function saveModel(val: string) {
    const before = savedModel;
    model = val;
    savedModel = val;
    await saveSetting({ model: val }, () => {
      model = before;
      savedModel = before;
    });
  }

  async function saveEffort(val: string) {
    const before = savedEffort;
    effort = val;
    savedEffort = val;
    await saveSetting({ effort: val }, () => {
      effort = before;
      savedEffort = before;
    });
  }

  async function savePermission(val: string) {
    const before = savedPermission;
    permission = val;
    savedPermission = val;
    await saveSetting({ permission_mode: val }, () => {
      permission = before;
      savedPermission = before;
    });
  }

  async function saveCrunchNow() {
    // Окно могло закрыться, пока ждали текст: адрес сессии берём до ожидания.
    const id = item.id;
    await seeded;
    const body = { enabled: crunch, resume_text: resumeText };
    await intents.run(`crunch:${id}:${JSON.stringify(body)}`, (key) => setCrunch(id, body, key));
  }

  async function saveCrunch() {
    await attempt('Не удалось сохранить текст продолжения', saveCrunchNow, () => undefined);
  }

  async function saveGroups(id: string, on: boolean) {
    const before = members;
    const conversation = item.id;
    members = on ? [...members, id] : members.filter((itemId) => itemId !== id);
    const body = { group_ids: members };
    await attempt(
      'Не удалось изменить группы',
      () => intents.run(`groups:${conversation}:${JSON.stringify(body)}`, (key) => setConversationGroups(conversation, body, key)),
      () => {
        members = before;
      }
    );
  }

  async function sessionNow(id: string): Promise<{ live: boolean; turn: string }> {
    const { data } = await getConversation(id);
    const execution = data.execution as { state?: string } | null;
    const activity = data.activity as unknown as Activity;
    return {
      live: execution !== null && !['stopped', 'failed'].includes(execution.state ?? ''),
      turn: turnRunning(activity) ? turnText(activity, Date.now()) : ''
    };
  }

  async function askDelete() {
    if (stopping) return;
    deleteError = '';
    const now = await sessionNow(item.id).catch(() => null);
    if (now?.turn) {
      deleteError = `Сначала остановите работу: ${now.turn}`;
      return;
    }
    stopFirst = now?.live ?? false;
    confirmingDelete = true;
  }

  async function stopProcess(id: string): Promise<boolean> {
    // Окно подтверждения могло стоять долго: начавшийся за это время ход удаление не обрывает.
    const before = await sessionNow(id);
    if (before.turn) {
      deleteError = `Сначала остановите работу: ${before.turn}`;
      return false;
    }
    if (!before.live) return true;
    stopping = true;
    await stopSession(id, { key: stopKey });
    for (let wait = 0; wait < STOP_POLLS; wait++) {
      if (!(await sessionNow(id)).live) return true;
      await new Promise((done) => setTimeout(done, STOP_POLL_MS));
    }
    deleteError = 'Процесс не остановился: машина не ответила. Попробуйте ещё раз';
    return false;
  }

  async function remove() {
    deleteError = '';
    const id = item.id;
    try {
      if (stopFirst && !(await stopProcess(id))) return;
      await deleteConversation(id, deleteKey);
      onclose();
      ondeleted?.(id);
    } catch (err) {
      deleteError = errorText(err, 'Не удалось удалить сессию');
    } finally {
      stopping = false;
    }
  }

  async function archive(force = false) {
    saveError = '';
    try {
      await patch({ archived: true, force: force || undefined });
      reasons = [];
      if (force) onclose();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'archive_active') {
        reasons = await reasonLabels(err.details?.reasons ?? []);
        return;
      }
      saveError = errorText(err, 'Не удалось архивировать');
    }
  }
</script>

{#snippet fields()}
  <div class="fields" bind:this={content}>
    <Field label={touch ? 'Переименовать' : 'Название · Enter сохраняет'}>
      {#snippet children({ id })}
        <input {id} bind:value={title} aria-label="Название" onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); void saveTitle(); } }} onblur={() => void saveTitle()} />
      {/snippet}
    </Field>
    <div class="line">
      <span class="caption">Цвет</span>
      <ColorSwatches label="Цвет" options={swatches} value={chosen} onchange={(key) => void saveColor(key)} />
    </div>
    <div class="rows">
      <Toggle block icon="pin" label="Закрепить" bind:checked={pinned} />
      <div class="groups">
        <div class="line">
          <span class="caption">Группы</span>
          {#if ongroups}<button type="button" class="link" onclick={ongroups}>Изменить список</button>{/if}
        </div>
        {#if groups.length > 0}
          <div class="chips">
            {#each groups as group (group.id)}
              <ChipToggle label={group.name} checked={members.includes(group.id)} onchange={(on) => void saveGroups(group.id, on)} />
            {/each}
          </div>
        {:else}
          <p class="note">Групп пока нет</p>
        {/if}
      </div>
      <Disclosure flat icon="sliders" title="Настройки сессии">
        <div class="fields">
          <div class="pair">
            <Select id="sheet-model" label="Модель" options={modelOptions} bind:value={model} onchange={() => void saveModel(model)} />
            <Select id="sheet-effort" label="Усилие" options={effortOptions} bind:value={effort} onchange={() => void saveEffort(effort)} />
          </div>
          <Select id="sheet-permission" label="Режим разрешений" hint={permissionHint(permission, harness?.provider ?? item.harness?.provider)} options={permissionOptions} bind:value={permission} onchange={() => void savePermission(permission)} />
          <Toggle block label="Автопродолжение после лимитов" bind:checked={crunch} />
          {#if crunch}
            <Textarea id="sheet-resume" label="Текст продолжения" rows={2} bind:value={resumeText} onblur={() => void saveCrunch()} />
          {/if}
        </div>
      </Disclosure>
    </div>
    <button type="button" class="archive" onclick={() => void archive(false)}>
      <Icon name="archive" /><span>В архив</span><small>История сохранится</small>
    </button>
    {#if reasons.length > 0}
      <p class="note">Сначала подтвердите: {reasons.join(', ')}</p>
      <Button onclick={() => void archive(true)}>Всё равно архивировать</Button>
    {/if}
    <button type="button" class="archive remove" onclick={() => void askDelete()}>
      <Icon name="trash" /><span>Удалить</span><small>Без возможности вернуть</small>
    </button>
    {#if saveError}<p class="error" role="alert">{saveError}</p>{/if}
    {#if stopping}<p class="note" role="status">Останавливаю процесс</p>{/if}
    {#if deleteError}<p class="error" role="alert">{deleteError}</p>{/if}
  </div>
{/snippet}

{#if confirmingDelete}
  <ConfirmDialog
    title={stopFirst ? 'Остановить и удалить сессию?' : 'Удалить сессию?'}
    body={`${stopFirst ? `Процесс агента на машине ${item.machine.name} будет остановлен. ` : ''}Сессия «${item.title}» будет удалена вместе с историей и вложениями. Это нельзя отменить. История CLI на машине останется: её удаляют средствами самого агента.`}
    confirmLabel={stopFirst ? 'Остановить и удалить' : 'Удалить'}
    danger
    onconfirm={() => { confirmingDelete = false; void remove(); }}
    onclose={() => { confirmingDelete = false; }}
  />
{/if}

{#if touch}
  <Sheet open title="Меню сессии" onclose={() => { void saveTitle(); onclose(); }}>{@render fields()}</Sheet>
{:else}
  <div class="popover" role="dialog" aria-label="Сессия" bind:this={panel} use:dismissable={{ onclose: dismissBy, keep: ownToggle }} style:top={`${top}px`} style:max-height={room === null ? undefined : `${room}px`}>
    <div class="close"><IconButton icon="close" label="Закрыть" onclick={dismiss} /></div>
    {@render fields()}
  </div>
{/if}

<style>
  .popover {
    position: absolute;
    z-index: 3;
    left: var(--s3);
    right: var(--s3);
    overflow-y: auto;
    padding: var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    animation: popover-enter var(--duration-fast) var(--ease);
  }
  @keyframes popover-enter { from { opacity: 0; transform: translateY(var(--shift-label)); } to { opacity: 1; transform: none; } }
  .close { position: absolute; top: var(--s1); right: var(--s1); }
  .fields { display: flex; flex-direction: column; gap: var(--s6); }
  .pair { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr)); gap: var(--s4); }
  .line { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); flex-wrap: wrap; }
  .caption { color: var(--secondary); font-size: var(--fs-meta); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); padding: var(--s2) var(--s6); border-radius: var(--radius-card); background: var(--raised); }
  .groups { display: flex; flex-direction: column; gap: var(--s2); padding-block: var(--s2); }
  .chips { display: flex; flex-wrap: wrap; gap: var(--s3); }
  .link { min-height: var(--target); padding-inline: var(--s2); border-radius: var(--radius-sm); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-meta); }
  .note { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
  .archive { display: flex; align-items: center; gap: var(--s6); width: 100%; min-height: var(--target); padding: var(--s2) var(--s6); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); text-align: left; }
  .archive span { flex: 1; }
  .remove { background: var(--danger-soft); color: var(--danger); }
  .error { margin: 0; color: var(--danger); font-size: var(--fs-sm); }
</style>
