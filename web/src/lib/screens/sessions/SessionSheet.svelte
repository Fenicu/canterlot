<script lang="ts">
  import { ApiError } from '#lib/api/http.js';
  import { getConversation, patchConversation, setConversationGroups, patchSettings, setCrunch } from '#lib/api/client.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import { untrack } from 'svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import { COLOR_KEYS, COLOR_NAMES } from './colors';

  let {
    item,
    groups,
    touch = isTouchPrimary(),
    onclose
  }: {
    item: ListRow;
    groups: { id: string; name: string }[];
    touch?: boolean;
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
  <label>Название <input bind:value={title} aria-label="Название" onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); void saveTitle(); } }} onblur={() => void saveTitle()} /></label>
  <div role="radiogroup" aria-label="Цвет">
    {#each COLOR_KEYS as key (key)}
      <label><input type="radio" name="color" value={key} checked={chosen === key} onchange={() => void saveColor(key)} />{COLOR_NAMES[key]}</label>
    {/each}
  </div>
  <Toggle label="Закрепить" bind:checked={pinned} />
  <div class="sheet-field">
    <label class="field-label" for="sheet-model">Модель</label>
    <select id="sheet-model" bind:value={model} onchange={() => void saveModel(model)}>
      <option value="opus">opus</option>
      <option value="opus-5.5">opus-5.5</option>
      <option value="sonnet-5">sonnet-5</option>
      <option value="haiku">haiku</option>
    </select>
  </div>
  <div class="sheet-field">
    <label class="field-label" for="sheet-effort">Усилие</label>
    <select id="sheet-effort" bind:value={effort} onchange={() => void saveEffort(effort)}>
      <option value="low">Низкое</option>
      <option value="medium">Обычное</option>
      <option value="high">Высокое</option>
    </select>
  </div>
  <div class="sheet-field">
    <label class="field-label" for="sheet-permission">Режим разрешений</label>
    <select id="sheet-permission" bind:value={permission} onchange={() => void savePermission(permission)}>
      <option value="default">По умолчанию</option>
      <option value="ask">Спрашивать</option>
      <option value="auto_edit">Автоправки</option>
      <option value="full_access">Полный доступ</option>
    </select>
  </div>
  <div class="sheet-field">
    <Toggle label="Автопродолжение после лимитов" bind:checked={crunch} />
    {#if crunch}
      <label class="field-label" for="sheet-resume">Текст продолжения</label>
      <textarea id="sheet-resume" rows="2" bind:value={resumeText} onblur={() => void saveCrunch()}></textarea>
    {/if}
  </div>
  <div>
    <p>Группы</p>
    {#each groups as group (group.id)}
      <label class="member">
        <input type="checkbox" checked={members.includes(group.id)} onchange={(event) => void saveGroups(group.id, event.currentTarget.checked)} />
        <span class="dot" class:filled={members.includes(group.id)} aria-hidden="true"></span>
        {group.name}
      </label>
    {/each}
  </div>
  <button type="button" onclick={() => void archive(false)}>Архив</button>
  {#if reasons.length > 0}
    <p>Сначала подтвердите: {reasons.join(', ')}</p>
    <button type="button" onclick={() => void archive(true)}>Всё равно архивировать</button>
  {/if}
{/snippet}

{#if touch}
  <Sheet open title="Сессия" {onclose}>{@render fields()}</Sheet>
{:else}
  <div class="popover" role="dialog" aria-label="Сессия">
    <button type="button" onclick={onclose}>Закрыть</button>
    {@render fields()}
  </div>
{/if}

<style>
  .popover {
    position: absolute;
    z-index: 2;
    right: var(--s6);
    width: min(100%, var(--phone-max));
    padding: var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
  }
  label, button { display: flex; align-items: center; gap: var(--s3); min-height: var(--target); }
  .member { position: relative; }
  input[type='checkbox'] { position: absolute; inset: 0; opacity: 0; margin: 0; }
  .dot { width: var(--mark-large); height: var(--mark-large); border-radius: var(--radius-round); border: var(--hairline) solid var(--text); pointer-events: none; }
  .filled { background: var(--accent); border-color: var(--accent); }
  button { border: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
