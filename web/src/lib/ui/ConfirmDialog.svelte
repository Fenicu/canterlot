<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import Sheet from './Sheet.svelte';
  import Button from './Button.svelte';
  let { title, body, confirmLabel, danger = false, onconfirm, onclose, open = true, routeKey }: {
    title: string; body: string; confirmLabel: string; danger?: boolean;
    onconfirm: () => void; onclose?: () => void; open?: boolean; routeKey?: string;
  } = $props();
  const autoKey = $props.id();
  let dismissed = $state(false);
  // Хозяин, который держит окно в документе и открывает его пропом `open`, получает то же окно снова:
  // без сброса оно после первого закрытия больше не открывалось.
  $effect(() => { if (!open) dismissed = false; });
  let confirmed = false;
  let alive = true;
  onDestroy(() => { alive = false; });
  /**
   * Подтверждённое действие ждёт, пока лист снимет свою запись истории: переход или лист, который оно откроет,
   * иначе попал бы под ещё не отработавший шаг назад. Хозяин узнаёт о закрытии после действия, как и раньше;
   * если действие уже убрало окно из документа, `onclose` не зовётся.
   */
  async function closed(released: Promise<void>) {
    dismissed = true;
    if (!confirmed) { onclose?.(); return; }
    confirmed = false;
    await released;
    onconfirm();
    await tick();
    if (alive) onclose?.();
  }
</script>
<Sheet open={open && !dismissed} {title} routeKey={routeKey ?? autoKey} onrevive={() => { dismissed = false; }} onclose={closed}>
  <p>{body}</p><div class:danger><Button variant="primary" onclick={() => { confirmed = true; dismissed = true; }}>{confirmLabel}</Button><Button variant="text" onclick={() => { dismissed = true; onclose?.(); }}>Отмена</Button></div>
</Sheet>
<style>
  div { display: flex; flex-wrap: wrap; gap: var(--s4); margin-top: var(--s8); }
  .danger { --accent: var(--danger); --accent-ink: var(--bg); }
</style>
