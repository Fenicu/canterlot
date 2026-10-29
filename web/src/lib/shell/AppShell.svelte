<script lang="ts">
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { clock } from '$lib/clock';
  import { browserRuntime } from '$lib/screens/chat/open';
  import SessionList from '$lib/screens/sessions/SessionList.svelte';
  import type { Runtime } from '$lib/app/runtime';
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import ConnectionBanner from './ConnectionBanner.svelte';
  import { layoutMode, type LayoutMode } from './layout';
  import Nav from './Nav.svelte';
  import SessionExpired from './SessionExpired.svelte';

  let { children }: { children: Snippet } = $props();
  let runtime = $state<Runtime | null>(null);
  let width = $state(0);
  let laidOut = $state(false);
  let tick = $state(0);
  let lane = $state(false);
  const mode: LayoutMode = $derived(layoutMode(width || 412));
  const path = $derived(page.url.pathname);
  const onSessions = $derived(path === '/' || path === '/sessions');
  const onChat = $derived(path.startsWith('/c/'));
  const onNew = $derived(path === '/new');
  const harness = $derived(path.startsWith('/__test'));
  const nav = $derived(path.startsWith('/machines') ? 'machines' : path.startsWith('/limits') ? 'limits' : 'sessions');
  const now = $derived.by(() => {
    void tick;
    return clock.serverNow();
  });

  onMount(() => {
    const read = () => {
      width = document.documentElement.clientWidth;
    };
    read();
    laidOut = true;
    window.addEventListener('resize', read);
    const timer = window.setInterval(() => tick++, 1000);
    void browserRuntime().then((value) => {
      runtime = value;
    });
    return () => {
      window.removeEventListener('resize', read);
      window.clearInterval(timer);
    };
  });
</script>

<div class="shell" data-layout={laidOut ? mode : undefined}>
  {#if runtime}
    <ConnectionBanner
      phase={runtime.connection.status.phase}
      since={runtime.connection.status.since}
      {now}
      scope={runtime.list.status}
      onrefresh={() => window.location.reload()}
    />
    <SessionExpired phase={runtime.connection.status.phase} {now} />
  {/if}
  {#if harness}
    <div class="page">{@render children()}</div>
  {:else if laidOut && mode === 'phone'}
    {#if onSessions || onNew}
      <SessionList main={onSessions} />
    {/if}
    {#if onChat}<a class="back" href={resolve('/sessions')}>Назад</a>{/if}
    {#if !onSessions}
      <div class="page">{@render children()}</div>
    {/if}
    {#if !onChat}
      <Nav current={nav} />
    {/if}
  {:else if laidOut}
    <div class="panes">
      <aside data-pane="list"><SessionList create /></aside>
      <div data-pane="chat">
        {#if mode === 'desktop' && onChat}
          <button type="button" onclick={() => (lane = !lane)}>{lane ? 'Скрыть поручения' : 'Поручения'}</button>
        {/if}
        {@render children()}
      </div>
      {#if mode === 'desktop' && lane}
        <aside data-pane="lane"><p>Поручения</p></aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .shell { height: 100dvh; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .page { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .page :global(main), [data-pane='chat'] :global(main) { flex: 1; min-height: 0; height: 100%; display: flex; flex-direction: column; }
  .back { min-height: var(--target); padding: var(--s4) var(--s6); color: var(--accent-link); }
  .panes { flex: 1; min-height: 0; display: flex; gap: var(--s6); padding: var(--s6); }
  [data-pane] { min-width: 0; min-height: 0; overflow: hidden; display: flex; flex-direction: column; border-radius: var(--radius-sheet); background: var(--surface); }
  [data-pane='list'] { width: var(--sidebar-width); flex: 0 0 var(--sidebar-width); }
  [data-pane='chat'] { flex: 1; display: flex; flex-direction: column; }
  [data-pane='lane'] { width: var(--lane-width); flex: 0 0 var(--lane-width); padding: var(--s6); }
  button { min-height: var(--target); border: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
