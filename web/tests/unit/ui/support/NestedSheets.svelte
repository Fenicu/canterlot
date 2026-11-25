<script lang="ts">
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { page } from '$app/state';
  import type { SheetHost } from './sheetHost.svelte.js';

  // Повторяет «Сеансы и вход»: лист сеанса с ключом истории, а подтверждение хозяин убирает из документа сам.
  let { host }: { host: SheetHost } = $props();
</script>

<Sheet open={host.details} title="Сеанс" routeKey="session-details" onclose={() => { host.details = false; host.log.push('details closed'); }}>
  <p>Подробности</p>
</Sheet>

{#if host.confirm}
  <ConfirmDialog
    title="Завершить сеанс?"
    body="Сеанс будет завершён"
    confirmLabel="Завершить"
    onconfirm={() => { host.log.push(`confirmed at ${page.state.sheet ?? 'page'}`); host.confirm = false; host.details = false; }}
    onclose={() => { host.log.push('confirm closed'); host.confirm = false; }}
  />
{/if}
