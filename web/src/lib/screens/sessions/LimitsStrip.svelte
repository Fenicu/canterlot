<script lang="ts">
  import { resolve } from '$app/paths';
  import { limitStrip } from './limitsText';
  import type { LimitThresholds, ProviderLimits } from '#lib/stores/limits.js';

  let {
    providers,
    now,
    thresholds,
    prefer = '',
    crunch = false
  }: {
    providers: ProviderLimits[];
    now: number;
    thresholds: LimitThresholds;
    prefer?: string;
    crunch?: boolean;
  } = $props();

  const view = $derived(limitStrip(providers, now, thresholds, { prefer, crunch }));
</script>

{#if providers.length > 0}
  <details class="limits" data-tone={view.tone}>
    <summary>
      {#if view.crunchLabel}{view.crunchLabel}{:else}{view.summary}{/if}
      {#if view.tone === 'reserve'}<span>резерв</span>{/if}
    </summary>
    <p>{view.details}</p>

    {#if view.staleNote}
      <p>{view.staleNote}</p>
    {/if}

    <a href={resolve('limits')}>История лимитов</a>
  </details>
{/if}

<style>
  .limits {
    margin: var(--s3) var(--s6);
    padding: var(--s4) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--surface);
  }

  summary {
    min-height: var(--target);
    cursor: pointer;
  }

  [data-tone="warning"] summary {
    color: var(--warning);
  }

  [data-tone="reserve"] summary {
    color: var(--warning);
  }

  p {
    white-space: pre-line;
    color: var(--secondary);
  }

  a {
    display: inline-flex;
    align-items: center;
    min-height: var(--target);
    color: var(--accent-link);
  }
</style>
