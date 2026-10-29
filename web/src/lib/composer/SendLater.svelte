<script lang="ts" module>
  export { presets, validateScheduleTime } from '$lib/composer/schedule';
</script>

<script lang="ts">
  import { fmt } from '$lib/format/ru';
  import { presets } from '$lib/composer/schedule';
  import { isTouchPrimary } from '$lib/composer/sendRules';
  import Button from '$lib/ui/Button.svelte';
  import IconButton from '$lib/ui/IconButton.svelte';
  import Sheet from '$lib/ui/Sheet.svelte';

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
    if (open && !armed) {
      armed = true;
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

  function confirm() {
    if (blocked || !Number.isFinite(chosen)) return;
    onpick(chosen);
    onclose();
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
        <input type="radio" name="when-{panelId}" checked={selected === index} onchange={() => choose(index)} />
        <span>{option.label}</span>
        <time datetime={new Date(option.at).toISOString()}>{fmt.clock(option.at)}</time>
      </label>
    {/each}
  </div>
  <div class="when">
    <label>Дата <input type="date" bind:value={date} oninput={editFields} /></label>
    <label>Время <input type="time" bind:value={time} oninput={editFields} /></label>
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
  <div class="popover" id={panelId} role="dialog" aria-labelledby="{panelId}-title">
    <header>
      <h2 id="{panelId}-title">Отправить позже</h2>
      <IconButton icon="close" label="Закрыть" onclick={onclose} />
    </header>
    {@render fields()}
  </div>
{/if}

<style>
  .popover {
    position: absolute;
    right: var(--s4);
    bottom: 100%;
    z-index: 2;
    width: min(100%, var(--phone-max));
    margin-bottom: var(--s2);
    padding: var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
    color: var(--text);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  header { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); }
  h2 { margin: 0; font-size: var(--fs-title); font-weight: var(--weight-medium); }
  .presets { display: flex; flex-direction: column; gap: var(--s2); }
  label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: var(--target);
  }
  .presets label { padding: var(--s2) var(--s4); border-radius: var(--radius-pill); background: var(--raised); }
  .presets label.on { background: var(--selected); }
  input[type='radio'] { accent-color: var(--accent); }
  .when { display: flex; flex-wrap: wrap; gap: var(--s4); }
  .when label { flex: 1; flex-direction: column; align-items: stretch; }
  input[type='date'], input[type='time'] {
    min-height: var(--target);
    border: var(--s0);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--text);
  }
  time { color: var(--secondary); font-variant-numeric: tabular-nums; }
  .zone { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
</style>
