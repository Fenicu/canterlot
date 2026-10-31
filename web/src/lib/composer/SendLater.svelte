<script lang="ts" module>
  export { presets, validateScheduleTime } from '#lib/composer/schedule.js';
</script>

<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { presets } from '#lib/composer/schedule.js';
  import { isTouchPrimary } from '#lib/composer/sendRules.js';
  import Button from '#lib/ui/Button.svelte';
  import { dismissable, type DismissReason } from '#lib/ui/dismiss.js';
  import Field from '#lib/ui/Field.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

  let {
    open,
    now,
    blocked = false,
    panelId,
    onclose,
    onpick
  }: {
    open: boolean;
    now: number;
    blocked?: boolean;
    panelId: string;
    onclose: () => void;
    onpick: (at: number) => void;
  } = $props();

  const touch = isTouchPrimary();
  const options = $derived(presets(now));
  let selected = $state(0);
  let date = $state('');
  let time = $state('');
  let armed = false;
  let openedAt = 0;
  let lastDown = 0;
  let opener: HTMLElement | null = null;

  function pad(value: number): string {
    return String(value).padStart(2, '0');
  }

  function sync(at: number) {
    const local = new Date(at);
    date = `${local.getFullYear()}-${pad(local.getMonth() + 1)}-${pad(local.getDate())}`;
    time = `${pad(local.getHours())}:${pad(local.getMinutes())}`;
  }

  function chosenAt(): number {
    if (selected >= 0) return options[selected]?.at ?? Number.NaN;
    return Date.parse(`${date}T${time}`);
  }

  const chosen = $derived(chosenAt());
  const clockLabel = $derived(Number.isFinite(chosen) ? fmt.clock(chosen) : '');

  $effect(() => {
    const down = () => { lastDown = performance.now(); };
    window.addEventListener('pointerdown', down, true);
    return () => window.removeEventListener('pointerdown', down, true);
  });

  $effect(() => {
    if (open && !armed) {
      armed = true;
      openedAt = performance.now();
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      selected = 0;
      sync(presets(now)[0]?.at ?? now);
    }
    if (!open) armed = false;
  });

  function choose(index: number) {
    selected = index;
    const at = options[index]?.at;
    if (at !== undefined) sync(at);
  }

  function editFields() {
    selected = -1;
  }

  function confirm(event?: MouseEvent) {
    if (blocked || !Number.isFinite(chosen)) return;
    // Палец, отпущенный после долгого нажатия, попадает на кнопку под ним: нажатие началось до открытия окна и не подтверждает время.
    if (event && event.detail > 0 && lastDown < openedAt) return;
    onpick(chosen);
    dismiss();
  }

  /** Всплывающее окно на рабочем столе закрывается и с клавиатуры, фокус возвращается туда, откуда его открыли. */
  function dismiss() {
    onclose();
    opener?.focus();
  }

  function dismissBy(reason: DismissReason) {
    if (reason === 'escape') dismiss();
    else onclose();
  }

  function zoneLine(at: number): string {
    const name =
      new Intl.DateTimeFormat('ru', { timeZoneName: 'long' })
        .formatToParts(at)
        .find((part) => part.type === 'timeZoneName')?.value ?? '';
    const offset = -new Date(at).getTimezoneOffset();
    const sign = offset >= 0 ? '+' : '-';
    const abs = Math.abs(offset);
    const hours = Math.floor(abs / 60);
    const minutes = abs % 60;
    const utc = minutes ? `UTC${sign}${hours}:${pad(minutes)}` : `UTC${sign}${hours}`;
    return name ? `${name} · ${utc}` : utc;
  }
</script>

{#snippet fields()}
  <div class="presets" role="radiogroup" aria-label="Быстрый выбор">
    {#each options as option, index (option.label)}
      <label class:on={selected === index}>
        <input class="sr-only" type="radio" name="when-{panelId}" checked={selected === index} onchange={() => choose(index)} />
        <span>{option.label}</span>
        <time datetime={new Date(option.at).toISOString()}>{fmt.clock(option.at)}</time>
      </label>
    {/each}
  </div>
  <div class="when">
    <Field label="Дата">{#snippet children({ id })}<input {id} type="date" bind:value={date} oninput={editFields} />{/snippet}</Field>
    <Field label="Время">{#snippet children({ id })}<input {id} type="time" bind:value={time} oninput={editFields} />{/snippet}</Field>
  </div>
  <p class="zone">{zoneLine(now)}</p>
  {#if blocked}
    <Button variant="primary" full disabled reason="Станет доступно, когда загрузятся файлы" onclick={confirm}>
      Запланировать на {clockLabel}
    </Button>
  {:else}
    <Button variant="primary" full onclick={confirm}>Запланировать на {clockLabel}</Button>
  {/if}
{/snippet}

{#if touch && open}
  <Sheet open title="Отправить позже" onclose={onclose}>
    {@render fields()}
  </Sheet>
{:else if open}
  <div class="popover" id={panelId} role="dialog" aria-labelledby="{panelId}-title" use:dismissable={{ onclose: dismissBy, keep: (target) => target.closest(`[aria-controls="${panelId}"]`) !== null }}>
    <header>
      <h2 id="{panelId}-title">Отправить позже</h2>
      <IconButton icon="close" label="Закрыть" onclick={dismiss} />
    </header>
    {@render fields()}
  </div>
{/if}

<style>
  .popover {
    --field-bg: var(--popover-hover);
    position: absolute;
    right: var(--gutter);
    bottom: 100%;
    z-index: 2;
    width: min(calc(100% - 2 * var(--gutter)), var(--phone-max));
    margin-bottom: var(--s2);
    padding: var(--s6);
    border-radius: var(--radius-card);
    background: var(--popover);
    color: var(--text);
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    animation: popover-enter var(--duration-fast) var(--ease);
  }
  @keyframes popover-enter { from { opacity: 0; transform: translateY(var(--shift-label)); } to { opacity: 1; transform: none; } }
  header { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); }
  h2 { margin: 0; font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .presets { display: flex; flex-direction: column; gap: var(--s1); margin-bottom: var(--s2); }
  .presets label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    font-size: var(--fs-meta);
    cursor: pointer;
  }
  .presets label.on { background: var(--popover-hover); color: var(--accent-link); }
  .presets label:has(input:focus-visible) { outline: var(--focus-width) solid var(--accent); }
  .when { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--gauge-width)), 1fr)); gap: var(--s4); margin-bottom: var(--s2); }
  time { color: var(--secondary); font-variant-numeric: tabular-nums; }
  .on time { color: inherit; }
  .zone { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
</style>
