<script lang="ts">
  import { resolve } from '$app/paths';
  import { leaseRows, type LeaseState } from '#lib/format/lease.js';

  // Очередь тяжёлых задач машины: кто держит гейт или сборку и сколько, кто ждёт и на каком месте.
  // Пустая или несвежая очередь (машина не в сети) не показывается вовсе.
  let { lease, available, titleOf, now }: { lease: LeaseState | null | undefined; available: boolean; titleOf: (id: string) => string | undefined; now: number } = $props();
  const rows = $derived(available ? leaseRows(lease, titleOf, now) : { holders: [], waiters: [] });
</script>

{#if rows.holders.length || rows.waiters.length}
  <section class="lease" aria-labelledby="lease-heading" data-lease-queue>
    <h2 id="lease-heading">Очередь тяжёлых задач</h2>
    {#if rows.holders.length}
      <h3>Выполняются</h3>
      <ul>
        {#each rows.holders as row (row.key)}
          <li data-lease-holder>
            {#if row.conversationId}<a href={resolve('/c/[id]', { id: row.conversationId })}>{row.name}</a>{:else}<span class="name">{row.name}</span>{/if}
            <small>{row.detail ? `${row.kind} · ${row.detail}` : row.kind}</small>
          </li>
        {/each}
      </ul>
    {/if}
    {#if rows.waiters.length}
      <h3>Ждут</h3>
      <ul>
        {#each rows.waiters as row (row.key)}
          <li data-lease-waiter>
            {#if row.conversationId}<a href={resolve('/c/[id]', { id: row.conversationId })}>{row.name}</a>{:else}<span class="name">{row.name}</span>{/if}
            <small>{`${row.detail} · ${row.kind}`}</small>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}

<style>
  h3 { margin-block: var(--s6) var(--s2); font-size: var(--fs-meta); font-weight: var(--weight-medium); color: var(--muted); }
  ul { margin: 0; padding: 0; list-style: none; }
  li { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: var(--s2) var(--s6); min-height: var(--target); padding: var(--s5) var(--s8); margin-bottom: var(--s2); background: var(--surface); border-radius: var(--radius-card); }
  li a, .name { min-width: 0; overflow-wrap: anywhere; }
  small { color: var(--muted); font-size: var(--fs-meta); font-variant-numeric: tabular-nums; white-space: nowrap; }
</style>
