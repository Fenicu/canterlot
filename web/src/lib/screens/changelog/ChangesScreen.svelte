<script lang="ts">
  import { parseChangelog } from '#lib/changelog.js';
  import { APP_VERSION } from '#lib/stores/appVersion.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import ChangelogText from './ChangelogText.svelte';
  import '../secondary.css';

  /** Вся история изменений: то же, что в CHANGES.rst, только читаемо. */
  const entries = parseChangelog();
</script>

<main id="main" tabindex="-1">
  <SecondaryHeader title="История изменений" />
  <div class="secondary-body"><div class="secondary-content">
    <p class="note">Установленная версия — <strong>{APP_VERSION}</strong></p>
    {#each entries as entry (entry.version)}
      <section data-changelog-entry>
        <div class="heading">
          <h2>{entry.version}</h2>
          {#if entry.version === APP_VERSION}<span class="current">установлена</span>{/if}
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
  </div></div>
</main>

<style>
  .heading { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s4); }
  .heading h2 { margin: var(--s0); }
  .current { padding: var(--s1) var(--s3); border-radius: var(--radius-sm); background: var(--accent-soft); color: var(--accent-link); font-size: var(--fs-xs); }
  .date { color: var(--muted); font-size: var(--fs-xs); }
  section { padding: var(--s8); margin-top: var(--s8); border: var(--hairline) solid var(--line); border-radius: var(--radius-card); }
  .section { display: grid; gap: var(--s2); margin-top: var(--s6); }
  .title { color: var(--secondary); font-size: var(--fs-xs); font-weight: var(--weight-medium); }
  ul { display: grid; gap: var(--s3); margin: var(--s0); padding: var(--s0); list-style: none; }
  li { display: flex; gap: var(--s4); color: var(--secondary); font-size: var(--fs-meta); }
  .dot { flex: none; width: var(--mark); height: var(--mark); margin-top: var(--s3); border-radius: var(--radius-round); background: var(--control-line); }
  p { margin: var(--s0) var(--s0) var(--s4); color: var(--secondary); font-size: var(--fs-meta); }
</style>
