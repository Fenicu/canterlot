<script lang="ts">
  import { onMount } from 'svelte';
  import { getClaudeAutoMemorySettings, putClaudeAutoMemorySettings } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import Segmented from '#lib/ui/Segmented.svelte';

  // Настройка живёт на хабе; действует на сессии «как у владельца» после их перезапуска.
  let mode = $state<'on' | 'off' | null>(null);
  let error = $state('');
  let busy = $state(false);

  async function load() {
    error = '';
    try { mode = (await getClaudeAutoMemorySettings()).data.claude_auto_memory; }
    catch { error = 'Не удалось загрузить настройку встроенной памяти. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); });

  async function save(next: 'on' | 'off') {
    if (busy || next === mode) return;
    const previous = mode;
    mode = next; busy = true; error = '';
    try { mode = (await putClaudeAutoMemorySettings({ claude_auto_memory: next }, crypto.randomUUID())).data.claude_auto_memory; }
    catch (err) {
      mode = previous;
      error = err instanceof ApiError ? err.message : 'Не удалось сохранить настройку. Повторите при восстановлении связи';
    } finally { busy = false; }
  }
</script>

<section data-claude-auto-memory>
  <h2>Встроенная память Claude</h2>
  {#if error}<p class="error" role="alert">{error}</p>{#if mode === null}<button type="button" onclick={load}>Повторить</button>{/if}{/if}
  {#if mode !== null}
    <Segmented name="claude-auto-memory" label="Встроенная память Claude" options={[{ value: 'on', label: 'Включена' }, { value: 'off', label: 'Выключена' }]} bind:value={() => mode ?? 'on', (value) => void save(value === 'off' ? 'off' : 'on')} />
    <p class="note">Изменение действует с ближайшего запуска сессии: уже работающие сессии остаются в прежнем режиме, пока их не запустят заново. Касается сессий в режиме «как у владельца»; воркеры старше 1.43 настройку не применяют</p>
  {:else if !error}
    <p>Загружаю настройку встроенной памяти…</p>
  {/if}
</section>

<style>
  section { margin-bottom: var(--s10); }
</style>
