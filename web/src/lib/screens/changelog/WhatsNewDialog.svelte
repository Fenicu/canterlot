<script lang="ts">
  import { resolve } from '$app/paths';
  import type { ChangelogEntry } from '#lib/changelog.js';
  import Button from '#lib/ui/Button.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import ChangelogText from './ChangelogText.svelte';

  let { entries, onclose }: { entries: ChangelogEntry[]; onclose: () => void } = $props();
</script>

<Sheet open title="Что нового" subtitle="Обновления, вышедшие с прошлого визита" {onclose}>
  <div class="entries">
    {#each entries as entry (entry.version)}
      <section>
        <div class="meta">
          <span class="version">{entry.version}</span>
          {#if entry.date}<span class="date">{entry.date}</span>{/if}
        </div>
        {#each entry.sections as section (section.title)}
          {#if section.items.length}
            <div class="section">
              <div class="title">{section.title}</div>
              <ul>
                {#each section.items as item (item)}
                  <li><span class="dot" aria-hidden="true"></span><span><ChangelogText text={item} /></span></li>
                {/each}
              </ul>
            </div>
          {/if}
        {/each}
        {#each entry.preamble as line (line)}
          <p><ChangelogText text={line} /></p>
        {/each}
      </section>
    {/each}
  </div>
  <div class="actions">
    <a href={resolve('/changes')} onclick={onclose}>Вся история изменений</a>
    <Button variant="primary" onclick={onclose}>Понятно</Button>
  </div>
</Sheet>

<style>
  .entries { display: grid; gap: var(--s10); }
  section { display: grid; gap: var(--s4); }
  .meta { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s4); }
  .version { padding: var(--s1) var(--s3); border-radius: var(--radius-sm); background: var(--accent-soft); color: var(--accent-link); font-size: var(--fs-xs); font-weight: var(--weight-medium); }
  .date { color: var(--muted); font-size: var(--fs-xs); }
  .section { display: grid; gap: var(--s2); }
  .title { color: var(--secondary); font-size: var(--fs-xs); font-weight: var(--weight-medium); }
  ul { display: grid; gap: var(--s3); margin: var(--s0); padding: var(--s0); list-style: none; }
  li { display: flex; gap: var(--s4); color: var(--secondary); font-size: var(--fs-meta); }
  .dot { flex: none; width: var(--mark); height: var(--mark); margin-top: var(--s3); border-radius: var(--radius-round); background: var(--control-line); }
  p { margin: var(--s0); color: var(--secondary); font-size: var(--fs-meta); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s4); margin-top: var(--s10); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
</style>
