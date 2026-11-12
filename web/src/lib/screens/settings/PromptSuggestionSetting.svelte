<script lang="ts">
  import { onMount } from 'svelte';
  import { getPromptSuggestionSettings, putPromptSuggestionSettings } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import Segmented from '#lib/ui/Segmented.svelte';

  // Настройка живёт на хабе: выключение сразу стирает показанные подсказки во всех сессиях.
  let enabled = $state<boolean | null>(null);
  let error = $state('');
  let busy = $state(false);

  async function load() {
    error = '';
    try { enabled = (await getPromptSuggestionSettings()).data.enabled; }
    catch { error = 'Не удалось загрузить настройку подсказок. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  async function save(next: boolean) {
    if (busy || next === enabled) return;
    const previous = enabled;
    enabled = next; busy = true; error = '';
    try { enabled = (await putPromptSuggestionSettings({ enabled: next }, crypto.randomUUID())).data.enabled; }
    catch (err) {
      enabled = previous;
      error = err instanceof ApiError ? err.message : 'Не удалось сохранить настройку. Повторите при восстановлении связи';
    } finally { busy = false; }
  }
</script>

<section data-prompt-suggestions>
  <h2>Подсказки следующего сообщения</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if enabled === null}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if enabled !== null}
    <Segmented name="prompt-suggestions" label="Подсказки следующего сообщения" options={[{ value: 'on', label: 'Включены' }, { value: 'off', label: 'Выключены' }]} bind:value={() => (enabled ? 'on' : 'off'), (value) => void save(value === 'on')} />
    <p class="note">Claude Code предлагает следующее сообщение серым в пустом поле ввода: Tab на компьютере или кнопка «Подставить» на телефоне вставляют его. Только в своих сессиях Claude Code; подсказка расходует немного лимита Claude</p>
  {:else if !error}
    <p>Загружаю настройку подсказок…</p>
  {/if}
</section>

<style>
  section { margin-bottom: var(--s10); }
</style>
