<script lang="ts">
  import { onMount, onDestroy, type Snippet } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import IconButton from './IconButton.svelte';
  import { leave } from './exit';
  // `marker` — приглушённая пометка в строке заголовка: заголовок тогда держится одной строкой и режется многоточием.
  // `feed` — лист с лентой чата: фон ленты, тело без своей прокрутки и отступов, прокручивает сама лента.
  let { open, title, subtitle, marker, feed = false, routeKey, onclose, actions, children }: { open: boolean; title: string; subtitle?: string; marker?: string; feed?: boolean; routeKey?: string; onclose: () => void; actions?: Snippet; children: Snippet } = $props();
  const id = $props.id();
  let dialog: HTMLDialogElement;
  let heading: HTMLHeadingElement | undefined;
  let mounted = $state(false);
  let opener: HTMLElement | null = null;
  let shell: HTMLElement | null = null;
  let previousInert = false;
  let entry = $state(false);
  function restore() {
    if (shell) shell.inert = previousInert;
    opener?.focus({ preventScroll: true });
  }
  /** Закрытый лист ещё доигрывает уход: скрыт для чтения с экрана и ничего не принимает. */
  function retire() {
    dialog.setAttribute('aria-hidden', 'true');
  }
  // Хозяин убрал лист из документа: закрыть сразу, а узел остаётся на время ухода (см. `ui/exit.ts`).
  function leaving(): boolean {
    const wasOpen = dialog.open;
    if (wasOpen) {
      dialog.close();
      restore();
    }
    return wasOpen;
  }
  function close() {
    if (!dialog.open) return;
    dialog.close();
    retire();
    restore();
    if (entry && routeKey && page.state.sheet === routeKey) history.back();
    entry = false;
    onclose();
  }
  function keydown(event: KeyboardEvent) {
    if (event.key === 'Escape') { event.preventDefault(); close(); }
    if (event.key !== 'Tab') return;

    const items = [
      ...dialog.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')
    ].filter((node) => node.getClientRects().length > 0);

    const first = items[0];
    const last = items.at(-1);

    if (event.shiftKey && (document.activeElement === first || document.activeElement === heading)) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  }
  // Нажатие должно и начаться за листом: выделение текста, отпущенное над фоном, лист не закрывает.
  let pressedBackdrop = false;
  function overBackdrop(event: MouseEvent): boolean {
    if (event.target !== dialog) return false;
    const box = dialog.getBoundingClientRect();
    return event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom;
  }
  function backdropClick(event: MouseEvent) {
    if (pressedBackdrop && overBackdrop(event)) close();
    pressedBackdrop = false;
  }
  onMount(() => { mounted = true; });
  onDestroy(() => { if (dialog?.open) restore(); });
  $effect(() => {
    if (!mounted) return;
    if (open && !dialog.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      shell = document.getElementById('app-shell');
      previousInert = shell?.inert ?? false;

      if (routeKey && page.state.sheet !== routeKey) {
        // eslint-disable-next-line svelte/no-navigation-without-resolve
        void goto('', { shallow: true, state: { ...page.state, sheet: routeKey } }).then(() => {
          entry = true;
        });
      }

      dialog.removeAttribute('aria-hidden');
      dialog.showModal();
      // Сам showModal ставит фокус на «Закрыть», и без указателя на нём загорается кольцо.
      // Заголовок принимает фокус тихо, а Tab ведёт дальше по листу.
      heading?.focus({ preventScroll: true });
      if (shell) shell.inert = true;
    } else if (!open && dialog.open) close();
  });
  $effect(() => { if (mounted && entry && routeKey && page.state.sheet !== routeKey) close(); });
</script>
<dialog bind:this={dialog} class:feed-sheet={feed} aria-labelledby={id} aria-modal="true" out:leave|global={{ before: leaving }} onkeydown={keydown} onpointerdown={(event) => { pressedBackdrop = overBackdrop(event); }} onclick={backdropClick} oncancel={(event) => { event.preventDefault(); close(); }}>
  <div class="handle" aria-hidden="true"></div>
  <header><div class="titles"><h2 id={id} bind:this={heading} tabindex="-1" class:line={marker}>{#if marker}<span class="name">{title}</span> <span class="marker">· {marker}</span>{:else}{title}{/if}</h2>{#if subtitle}<p>{subtitle}</p>{/if}</div><div class="sheet-actions">{#if actions}{@render actions()}{/if}<IconButton icon="close" label="Закрыть" onclick={close} /></div></header>
  <div class="body">{@render children()}</div>
</dialog>
<style>
  dialog { position: fixed; inset: auto 0 0; width: min(100%, var(--reading-max)); max-width: 100%; max-height: var(--sheet-max); margin: auto auto 0; padding: var(--s4) var(--s8) var(--s0); border: 0; border-radius: var(--radius-sheet) var(--radius-sheet) 0 0; background: var(--surface); color: var(--text); overflow: hidden; }
  /* Раскладка столбцом не привязана к `[open]`: уходящий лист уже закрыт, но ещё виден и стоит так же. */
  dialog { flex-direction: column; }
  dialog[open] { display: flex; }
  dialog::backdrop { background: var(--scrim-soft); }
  dialog[open] { animation: sheet-enter var(--duration-expand) var(--ease-sheet); }
  .handle { width: var(--handle-width); height: var(--handle-height); border-radius: var(--radius-pill); background: var(--control-line); margin: var(--s4) auto; }
  header { flex: none; display: flex; align-items: center; justify-content: space-between; gap: var(--s4); }
  .sheet-actions { flex: none; display: flex; align-items: center; gap: var(--s4); }
  .titles { min-width: 0; }
  h2:focus { outline: none; }
  p { margin: var(--s1) var(--s0) var(--s0); color: var(--muted); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  /* Запас по бокам вмещает кольцо фокуса полей во всю ширину: прокрутка обрезала бы его. */
  /* Тело листа прокручивается только вниз: иначе `overflow-y: auto` делает его ещё и
     горизонтальным скроллером, и на телефоне палец достаётся ему вместо широкой таблицы внутри. */
  .body { --ring-room: calc(var(--focus-width) + var(--s1)); flex: 1 1 auto; min-height: 0; padding-block: var(--s6) calc(var(--s8) + env(safe-area-inset-bottom)); padding-inline: var(--ring-room); margin-inline: calc(-1 * var(--ring-room)); overflow-x: hidden; overflow-y: auto; overscroll-behavior: contain; mask-image: linear-gradient(to bottom, var(--transparent), var(--text) var(--s6)); }
  :global([data-layout='desktop']) dialog { inset: 0; height: fit-content; margin: auto; padding: var(--s6) var(--s10) var(--s0); border-radius: var(--radius-sheet); }
  :global([data-layout='desktop']) .handle { display: none; }
  .body:has(> :global([data-sheet-flush])) { padding-bottom: var(--s0); }
  :global([data-layout='desktop']) .body { padding-bottom: var(--s10); }
  :global([data-layout='desktop']) dialog[open] { animation-name: sheet-rise; }
  h2.line { display: flex; align-items: baseline; gap: var(--s1); min-width: 0; white-space: nowrap; }
  .name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .marker { flex: none; color: var(--muted); font-size: var(--fs-meta); font-weight: var(--weight-normal); }
  .feed-sheet { padding-inline: var(--s0); background: var(--chat-bg); }
  .feed-sheet header { min-height: var(--target); padding-inline: var(--s8) var(--s4); }
  .feed-sheet .body { display: flex; flex-direction: column; padding: var(--s0); margin: var(--s0); overflow: hidden; mask-image: none; }
  /*
   * Уход листа: тем же путём, каким пришёл, но быстрее. Лист закрывается сразу (фокус, история и страница
   * под ним свободны с первого кадра), а `display` и `overlay` меняются дискретно в конце перехода,
   * поэтому закрытый лист остаётся в верхнем слое, пока доезжает. Кривая входа отдаёт почти весь путь
   * первому кадру и на выходе лист просто пропал бы, потому уход идёт по `--ease-exit`.
   * Где `overlay` не поддержан (Firefox), закрытый лист выходит из верхнего слоя сразу и рисовать его
   * уход нельзя: без этого блока он закрывается мгновенно, как раньше. Без движения токен 0 мс.
   */
  @supports (overlay: auto) {
    dialog { pointer-events: none; transform: translateY(100%); transition: transform var(--duration-fast) var(--ease-exit), overlay var(--duration-fast) allow-discrete, display var(--duration-fast) allow-discrete; }
    dialog[open] { pointer-events: auto; transform: none; }
    dialog::backdrop { opacity: 0; transition: opacity var(--duration-fast) var(--ease-exit), overlay var(--duration-fast) allow-discrete, display var(--duration-fast) allow-discrete; }
    dialog[open]::backdrop { opacity: var(--opacity-high); }
    :global([data-layout='desktop']) dialog { opacity: 0; transform: translateY(var(--shift-enter)); transition: transform var(--duration-fast) var(--ease-exit), opacity var(--duration-fast) var(--ease-exit), overlay var(--duration-fast) allow-discrete, display var(--duration-fast) allow-discrete; }
    :global([data-layout='desktop']) dialog[open] { opacity: var(--opacity-high); transform: none; }
  }
  @keyframes sheet-enter { from { transform: translateY(100%); } to { transform: none; } }
  @keyframes sheet-rise { from { opacity: 0; transform: translateY(var(--shift-enter)); } to { opacity: 1; transform: none; } }
</style>
