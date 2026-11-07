<script lang="ts">
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import { acceptFacts, acceptWord, type AcceptData } from './defs/sessions';
  import type { CardProps } from './types';

  let { call, data, line, link, env }: CardProps<AcceptData> = $props();
  // «останавливается» сменяется на «остановлена», когда ребёнок остановился: строка списка живая.
  const word = $derived(acceptWord(call, data));
  const facts = $derived(acceptFacts(data));
</script>

<CardShell {call} {env} {line} status={word.word} tone={word.tone} dot live={word.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if facts.length > 0}<p class="meta" data-role="accept-facts">{facts.join(' · ')}</p>{/if}
  {#if data.error}<p class="meta danger">{data.error}</p>{/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .danger { color: var(--danger); }
</style>
