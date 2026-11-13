<script lang="ts">
  import Button from '#lib/ui/Button.svelte';
  import { ticker } from '#lib/ticker.svelte.js';
  import type { DictationView } from '#lib/dictation/session.js';

  let {
    view,
    onfinish,
    oncancel,
    onpartial
  }: { view: DictationView; onfinish: () => void; oncancel: () => void; onpartial: () => void } = $props();

  const elapsed = $derived.by(() => {
    void ticker.now;
    return Math.max(0, Math.floor((ticker.now - view.startedAt) / 1000));
  });
  const clock = $derived(
    `${String(Math.floor(elapsed / 60)).padStart(2, '0')}:${String(elapsed % 60).padStart(2, '0')}`
  );
  const bars = $derived(view.level <= 0.01 ? 0 : Math.min(5, Math.ceil(view.level * 5)));
  const hasText = $derived(Boolean(view.committed || view.tentative));
  const placeholder = $derived(
    view.phase === 'starting'
      ? 'Разрешите доступ к микрофону'
      : view.phase === 'loading'
        ? 'Звук уже записывается'
        : view.phase === 'listening'
          ? 'Говорите'
          : ''
  );
  const levelLabel = $derived(`Уровень входа: ${bars} из 5`);
</script>

<div class="dictation" data-dictation data-phase={view.phase} role="group" aria-label="Диктовка">
  <div class="head">
    <p class="state" role="status" aria-live="polite" data-dictation-state>{view.message}</p>
    <p class="timer" data-dictation-timer aria-label={`Длительность записи ${clock}`}>{clock}</p>
    <span class="level" role="img" data-dictation-level data-level={bars} aria-label={levelLabel}>
      {#each [1, 2, 3, 4, 5] as bar (bar)}
        <span class="bar" class:on={bars >= bar}></span>
      {/each}
    </span>
  </div>
  <div class="transcript" data-dictation-transcript aria-label="Распознанный текст">
    {#if hasText}
      {#if view.committed}<span class="committed" data-dictation-committed>{view.committed}</span>{/if}
      {#if view.tentative}<span class="tentative" data-dictation-tentative>{view.tentative}</span>{/if}
    {:else}
      <span class="placeholder">{placeholder}</span>
    {/if}
  </div>
  {#if view.mode === 'cpu' && view.phase === 'listening'}
    <p class="note" data-dictation-slow-mode>Текст появляется после пауз</p>
  {/if}
  <div class="actions">
    {#if view.phase === 'review'}
      <Button variant="text" onclick={oncancel}>Отмена</Button>
      <Button variant="primary" onclick={onpartial}>Вставить что есть (неполно)</Button>
    {:else if view.phase === 'finishing'}
      <Button variant="text" onclick={oncancel}>Отмена</Button>
    {:else}
      <Button variant="text" onclick={oncancel}>Отмена</Button>
      <Button variant="primary" onclick={onfinish}>Готово</Button>
    {/if}
  </div>
</div>

<style>
  .dictation {
    grid-area: 1 / 1 / -1 / -1;
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    min-width: 0;
    min-height: calc(var(--target) * 2);
    padding: var(--s5) var(--s5) var(--s3);
  }
  .head { display: flex; align-items: center; gap: var(--s5); min-width: 0; }
  .state { flex: 1; min-width: 0; margin: var(--s0); color: var(--secondary); font-size: var(--fs-meta); }
  .timer { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); font-variant-numeric: tabular-nums; }
  .level { display: inline-flex; align-items: flex-end; gap: var(--s1); height: var(--s8); }
  .bar { width: var(--s2); height: var(--s2); border-radius: var(--radius-round); background: var(--control-line); transition: height var(--duration-fast) var(--ease), background var(--duration-fast) var(--ease); }
  .bar:nth-child(2) { height: var(--s3); }
  .bar:nth-child(3) { height: var(--s4); }
  .bar:nth-child(4) { height: var(--s5); }
  .bar:nth-child(5) { height: var(--s6); }
  .bar.on { background: var(--accent); }
  .transcript {
    min-height: calc(var(--fs-input) * var(--lh-body) * 2);
    max-height: calc(var(--fs-input) * var(--lh-body) * 5);
    overflow: auto;
    padding: var(--s4) var(--s5);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--text);
    font-size: var(--fs-input);
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .committed { color: var(--text); }
  .tentative { color: var(--muted); }
  .placeholder { color: var(--muted); }
  .note { margin: var(--s0) var(--s5); color: var(--muted); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: var(--s4); }
  .actions :global(button) { flex: 0 0 auto; }
</style>
