<script lang="ts">
  import { onMount } from 'svelte';
  import { getHibernateSettings, putHibernateSettings } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import Select from '#lib/ui/Select.svelte';

  // Настройка живёт на хабе: `null` — «Выключено», остальные значения — минуты простоя.
  let minutes = $state<number | null | undefined>(undefined);
  let error = $state('');
  let busy = $state(false);

  const options = [
    { value: 'off', label: 'Выключено' },
    { value: '15', label: 'Через 15 минут' },
    { value: '30', label: 'Через 30 минут' },
    { value: '60', label: 'Через 60 минут' },
    { value: '120', label: 'Через 2 часа' },
    { value: '240', label: 'Через 4 часа' }
  ];

  async function load() {
    error = '';
    try { minutes = (await getHibernateSettings()).data.idle_minutes ?? null; }
    catch { error = 'Не удалось загрузить настройку усыпления. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  async function save(next: number | null) {
    if (busy || next === minutes) return;
    const previous = minutes;
    minutes = next; busy = true; error = '';
    try {
      minutes = (await putHibernateSettings({ idle_minutes: next }, crypto.randomUUID())).data.idle_minutes ?? null;
    } catch (err) {
      minutes = previous;
      error = err instanceof ApiError ? err.message : 'Не удалось сохранить настройку. Повторите при восстановлении связи';
    } finally { busy = false; }
  }

  function selected(): string {
    return minutes === null || minutes === undefined ? 'off' : String(minutes);
  }
</script>

<section data-hibernate-settings>
  <h2>Усыпление простаивающих сессий</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if minutes === undefined}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if minutes !== undefined}
    <Select
      label="Усыплять после простоя"
      name="hibernate-idle-minutes"
      {options}
      value={selected()}
      onchange={(event) => void save(event.currentTarget.value === 'off' ? null : Number(event.currentTarget.value))}
    />
    <p class="note">Хаб остановит процесс давно неиспользуемой сессии, у которой нет фоновых работ, активных подписок и открытого чата. Следующее сообщение само возобновит беседу; первый ответ может прийти чуть позже. По умолчанию — через 60 минут</p>
  {:else if !error}
    <p>Загружаю настройку усыпления…</p>
  {/if}
</section>

<style>
  section { margin-bottom: var(--s10); }
</style>
