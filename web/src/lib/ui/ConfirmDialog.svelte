<script lang="ts">
  import Sheet from './Sheet.svelte';
  import Button from './Button.svelte';
  let { title, body, confirmLabel, danger = false, onconfirm, onclose, open = true }: { title: string; body: string; confirmLabel: string; danger?: boolean; onconfirm: () => void; onclose?: () => void; open?: boolean } = $props();
  let dismissed = $state(false);
</script>
<Sheet open={open && !dismissed} {title} onclose={() => { dismissed = true; onclose?.(); }}>
  <p>{body}</p><div class:danger><Button variant="primary" onclick={() => { onconfirm(); dismissed = true; }}>{confirmLabel}</Button><Button variant="text" onclick={() => { dismissed = true; onclose?.(); }}>Отмена</Button></div>
</Sheet>
<style>
  div { display: flex; flex-wrap: wrap; gap: var(--s4); margin-top: var(--s8); }
  .danger { --accent: var(--danger); --accent-ink: var(--bg); }
</style>
