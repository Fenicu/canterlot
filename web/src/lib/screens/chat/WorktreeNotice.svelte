<script lang="ts">
  import { DIRTY_NOTE, FORCE_LABEL } from './accept';

  let { onforce, error = null }: { onforce: () => void; error?: string | null } = $props();
</script>

<section class="notice" data-worktree-dirty aria-label="Рабочая копия">
  <p class="text">{DIRTY_NOTE}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <button type="button" class="force" onclick={onforce}>{FORCE_LABEL}</button>
</section>

<style>
  .notice {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--s3);
    margin: var(--s0) var(--gutter) var(--s3);
    padding: var(--s4);
    border-radius: var(--radius-card);
    background: var(--warning-soft);
    color: var(--text);
    min-width: 0;
  }
  .text, .error { margin: var(--s0); font-size: var(--fs-body); overflow-wrap: anywhere; }
  .error { color: var(--danger); font-size: var(--fs-meta); }
  .force {
    min-height: var(--target);
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--danger);
    font-size: var(--fs-body);
    font-weight: var(--weight-medium);
    cursor: pointer;
  }
  .force:focus-visible { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
</style>
