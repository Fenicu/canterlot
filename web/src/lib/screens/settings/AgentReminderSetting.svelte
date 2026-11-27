<script lang="ts">
  import { onMount } from 'svelte';
  import { getAgentReminderSettings, putAgentReminderSettings } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import Segmented from '#lib/ui/Segmented.svelte';

  type Reminders = { plan_enabled: boolean; todo_enabled: boolean };

  // Настройка живёт на хабе и действует со следующего сообщения агенту.
  let settings = $state<Reminders | null>(null);
  let error = $state('');
  let busy = $state(false);

  async function load() {
    error = '';
    try { settings = (await getAgentReminderSettings()).data; }
    catch { error = 'Не удалось загрузить настройку напоминаний. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  async function save(patch: Partial<Reminders>) {
    if (busy || !settings) return;
    const previous = settings;
    const next = { ...settings, ...patch };
    if (next.plan_enabled === previous.plan_enabled && next.todo_enabled === previous.todo_enabled) return;
    settings = next; busy = true; error = '';
    try { settings = (await putAgentReminderSettings(next, crypto.randomUUID())).data; }
    catch (err) {
      settings = previous;
      error = err instanceof ApiError ? err.message : 'Не удалось сохранить настройку. Повторите при восстановлении связи';
    } finally { busy = false; }
  }

  const onOff = [{ value: 'on', label: 'Включено' }, { value: 'off', label: 'Выключено' }];
</script>

<section data-agent-reminders>
  <h2>Напоминания агентам</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if settings === null}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if settings}
    <h3>Напоминать оркестратору об устаревших задачах плана</h3>
    <Segmented name="plan-reminders" label="Напоминать оркестратору об устаревших задачах плана" options={onOff} bind:value={() => (settings?.plan_enabled ? 'on' : 'off'), (value) => void save({ plan_enabled: value === 'on' })} />
    <p class="note">Задачи, которые давно ждут приёма, остались в работе без исполнителя или не менялись сутки, и задачи, чья ветка уже слита. Не чаще раза в три часа, одной строкой перед следующим сообщением</p>
    <h3>Напоминать агенту о списке задач</h3>
    <Segmented name="todo-reminders" label="Напоминать агенту о списке задач" options={onOff} bind:value={() => (settings?.todo_enabled ? 'on' : 'off'), (value) => void save({ todo_enabled: value === 'on' })} />
    <p class="note">Если пункт списка в работе, а список не менялся три хода, или он не менялся десять ходов. Одна строка на версию списка, не чаще раза в три часа</p>
  {:else if !error}
    <p>Загружаю настройку напоминаний…</p>
  {/if}
</section>

<style>
  section { margin-bottom: var(--s10); }
</style>
