<script lang="ts">
  import { resolve } from '$app/paths';
  import BottomNav from '#lib/ui/BottomNav.svelte';
  let { current, create = true }: { current: 'sessions' | 'machines' | 'limits' | 'presets'; create?: boolean } = $props();
</script>

<div class="dock">
  <BottomNav
    label="Навигация"
    items={[
      { href: '/sessions', label: 'Сессии', current: current === 'sessions' },
      { href: '/presets', label: 'Инструкции', current: current === 'presets' },
      { href: '/machines', label: 'Машины', current: current === 'machines' },
      { href: '/limits', label: 'Лимиты', current: current === 'limits' }
    ]}
  />
  {#if create}<a class="fab" href={resolve('/new')} aria-label="Новая сессия"><span aria-hidden="true">+</span></a>{/if}
</div>

<style>
  .dock { display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--s2); min-width: 0; }
  .dock :global(nav) { flex: 1; min-width: 0; flex-wrap: wrap; }
  .dock :global(nav a) { flex: 1 1 auto; padding-inline: var(--s3); }
  .fab {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--fab-size);
    height: var(--fab-size);
    margin: var(--s4) var(--s6) var(--s6) 0;
    border-radius: var(--radius-round);
    background: var(--accent);
    color: var(--accent-ink);
    font-size: var(--fs-title);
    flex-shrink: 0;
  }
</style>
