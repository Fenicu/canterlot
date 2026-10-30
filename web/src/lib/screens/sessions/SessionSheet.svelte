<script lang="ts">
  import { ApiError } from '#lib/api/http.js';
  import { getConversation, patchConversation, setConversationGroups, patchSettings, setCrunch } from '#lib/api/client.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import { effortLabel, permissionLabel } from '#lib/screens/chat/settingsLabels.js';
  import { onMount, untrack } from 'svelte';
  import Button from '#lib/ui/Button.svelte';
  import ChipToggle from '#lib/ui/ChipToggle.svelte';
  import ColorSwatches from '#lib/ui/ColorSwatches.svelte';
  import Disclosure from '#lib/ui/Disclosure.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import type { MachineRecord } from '#lib/stores/machines.svelte.js';
  import { COLOR_KEYS, COLOR_NAMES } from './colors';

  let {
    item,
    groups,
    machine = null,
    touch = isTouchPrimary(),
    anchor = 0,
    ongroups,
    onclose
  }: {
    item: ListRow;
    groups: { id: string; name: string }[];
    machine?: MachineRecord | null;
    touch?: boolean;
    anchor?: number;
    ongroups?: () => void;
    onclose: () => void;
  } = $props();

  const seed = untrack(() => item);
  let title = $state(seed.title);
  let pinned = $state(seed.pinned);
  let chosen = $state(seed.color ?? 'neutral');
  let members = $state(seed.groups.slice());
  let model = $state(seed.model ?? 'opus');
  let effort = $state(seed.effort ?? 'medium');
  let permission = $state(seed.permission_mode ?? 'default');
  let crunch = $state(seed.crunch?.enabled ?? false);
  let resumeText = $state('Лимиты восстановлены, продолжай с того места, где остановился.');
  let reasons = $state<string[]>([]);
  let sentPin = seed.pinned;
  let sentCrunch = seed.crunch?.enabled ?? false;
  // В строке списка текста продолжения нет, а хаб заменил бы сохранённый текст присланным.
  const seeded = getConversation(seed.id)
    .then((detail) => {
      if (detail.data.crunch.resume_text) resumeText = detail.data.crunch.resume_text;
    })
    .catch(() => undefined);

  $effect(() => {
    const value = pinned;
    if (value === sentPin) return;
    sentPin = value;
    void patch({ pinned: value });
  });
  $effect(() => {
    const value = crunch;
    if (value === sentCrunch) return;
    sentCrunch = value;
    void saveCrunch();
  });
  const harness = $derived(machine?.harnesses.find((entry) => entry.id === item.harness.id));
  const known = (values: string[], current: string) => (values.includes(current) ? values : [current, ...values]);
  const modelOptions = $derived(known(harness?.models.map((entry) => entry.id) ?? [], model).map((value) => ({ value, label: harness?.models.find((entry) => entry.id === value)?.title || value })));
  const effortOptions = $derived(known(harness?.models.find((entry) => entry.id === model)?.efforts ?? ['low', 'medium', 'high'], effort).map((value) => ({ value, label: effortLabel(value) })));
  const permissionOptions = $derived(known(harness?.permission_modes ?? ['default', 'ask', 'auto_edit', 'full_access'], permission).map((value) => ({ value, label: permissionLabel(value) })));
  const swatches = COLOR_KEYS.map((key) => ({ key, name: COLOR_NAMES[key] }));

  let panel = $state<HTMLElement | null>(null);
  let top = $state(untrack(() => anchor));
  let room = $state<number | null>(null);
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  onMount(() => {
    const parent = panel?.offsetParent;
    if (!panel || !(parent instanceof HTMLElement)) return;
    const height = parent.clientHeight;
    // Окно остаётся под своей строкой и прокручивается внутри; вверх уходит, только если снизу почти нет места.
    const fits = height - anchor >= Math.min(panel.offsetHeight, height / 2);
    top = fits ? anchor : Math.max(0, height - panel.offsetHeight);
    room = height - top;
  });

  function dismiss() {
    onclose();
    opener?.focus();
  }

  const reasonText: Record<string, string> = {
    crunch: 'кранч',
    scheduled: 'отложенные сообщения',
    live_execution: 'идёт исполнение'
  };

  async function patch(body: { title?: string; color?: string; pinned?: boolean; archived?: boolean; force?: boolean }, key = crypto.randomUUID()) {
    await patchConversation(item.id, body, key);
  }

  async function saveTitle() {
    if (title.trim() === item.title) return;
    await patch({ title: title.trim() });
  }

  async function saveColor(value: string) {
    chosen = value;
    await patch({ color: value });
  }

  async function saveModel(val: string) {
    model = val;
    await patchSettings(item.id, { model: val }, crypto.randomUUID());
  }

  async function saveEffort(val: string) {
    effort = val;
    await patchSettings(item.id, { effort: val }, crypto.randomUUID());
  }

  async function savePermission(val: string) {
    permission = val;
    await patchSettings(item.id, { permission_mode: val }, crypto.randomUUID());
  }

  async function saveCrunch() {
    await seeded;
    await setCrunch(item.id, { enabled: crunch, resume_text: resumeText }, crypto.randomUUID());
  }

  async function saveGroups(id: string, on: boolean) {
    members = on ? [...members, id] : members.filter((itemId) => itemId !== id);
    await setConversationGroups(item.id, { group_ids: members }, crypto.randomUUID());
  }

  async function archive(force = false) {
    try {
      await patch({ archived: true, force: force || undefined });
      reasons = [];
      if (force) onclose();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'archive_active') {
        reasons = (err.details?.reasons ?? []).map((reason) => reasonText[reason] ?? reason);
        return;
      }
      throw err;
    }
  }
</script>

{#snippet fields()}
  <div class="fields">
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
          <Select id="sheet-permission" label="Режим разрешений" options={permissionOptions} bind:value={permission} onchange={() => void savePermission(permission)} />
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
  </div>
{/snippet}

<svelte:window onkeydown={(event) => { if (!touch && event.key === 'Escape') dismiss(); }} />

{#if touch}
  <Sheet open title="Меню сессии" {onclose}>{@render fields()}</Sheet>
{:else}
  <div class="popover" role="dialog" aria-label="Сессия" bind:this={panel} style:top={`${top}px`} style:max-height={room === null ? undefined : `${room}px`}>
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
</style>
