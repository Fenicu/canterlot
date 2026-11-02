<script lang="ts">
  import { untrack } from 'svelte';
  import { getOwnerFile, getOwnerOverview } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import { createMarkdown } from '#lib/render/markdown.js';
  import Html from '#lib/render/Html.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { formatOwnerMtime, formatSize, OWNER_KIND_META, ownerHarnessSummary } from './presetsModel.js';

  let {
    machines,
    initialMachine,
    onclose,
  }: {
    machines: { id: string; name: string }[];
    initialMachine: string;
    onclose: () => void;
  } = $props();

  const engine = createMarkdown();
  let machineId = $state(untrack(() => initialMachine));
  let loading = $state(true);
  let error = $state('');
  let overview = $state<{
    files: { kind: string; harness: string; path: string; present: boolean; size: number; mtime: string | null }[];
    memory_projects: { id: string; files: number; has_index: boolean }[];
  } | null>(null);
  let expanded = $state<string | null>(null);
  let contents = $state<
    Record<string, { loading: boolean; error: string; text: string; truncated: boolean; files?: { name: string; size: number; mtime: string | null }[] }>
  >({});
  let projectId = $state('');
  let memory = $state<{ loading: boolean; error: string } | null>(null);

  const summary = $derived(overview ? ownerHarnessSummary(overview.files) : []);
  const machineName = $derived(machines.find((m) => m.id === machineId)?.name ?? '');

  async function loadOverview(): Promise<void> {
    loading = true;
    error = '';
    overview = null;
    contents = {};
    expanded = null;
    projectId = '';
    try {
      const response = await getOwnerOverview(machineId);
      const files = response.data.files.map((f) => ({
        kind: f.kind,
        harness: f.harness,
        path: f.path,
        present: f.present,
        size: f.size,
        mtime: f.mtime ?? null,
      }));
      const memory_projects = [...response.data.memory_projects];
      overview = { files, memory_projects };
      const first = memory_projects[0];
      if (memory_projects.length === 1 && first) projectId = first.id;
    } catch (err) {
      error =
        err instanceof ApiError && err.code === 'capability_missing'
          ? 'Воркер устарел: обновите его, чтобы посмотреть конфигурацию'
          : err instanceof ApiError && err.code === 'machine_unavailable'
            ? 'Машина недоступна: лист появится при живом воркере'
            : 'Не удалось загрузить конфигурацию';
    } finally {
      loading = false;
    }
  }

  async function toggle(kind: string): Promise<void> {
    if (expanded === kind) {
      expanded = null;
      return;
    }
    expanded = kind;
    if (contents[kind]) return;
    contents[kind] = { loading: true, error: '', text: '', truncated: false };
    try {
      const response = await getOwnerFile(machineId, { kind });
      contents[kind] = {
        loading: false,
        error: '',
        text: response.data.content,
        truncated: response.data.truncated,
      };
    } catch {
      contents[kind] = { loading: false, error: 'Не удалось прочитать файл', text: '', truncated: false };
    }
    contents = { ...contents };
  }

  async function loadMemory(name: string): Promise<void> {
    if (!projectId) return;
    const key = `memory:${name}`;
    if (contents[key]) {
      expanded = key;
      return;
    }
    expanded = key;
    contents[key] = { loading: true, error: '', text: '', truncated: false };
    try {
      const kind = name === 'MEMORY.md' ? 'memory_index' : 'memory_file';
      const response = await getOwnerFile(machineId, { kind, project: projectId, name });
      contents[key] = {
        loading: false,
        error: '',
        text: response.data.content,
        truncated: response.data.truncated,
        files: response.data.files.map((f) => ({ name: f.name, size: f.size, mtime: f.mtime ?? null })),
      };
    } catch {
      contents[key] = { loading: false, error: 'Не удалось прочитать файл', text: '', truncated: false };
    }
    contents = { ...contents };
  }

  $effect(() => {
    void machineId;
    void loadOverview();
  });
</script>

<Sheet open title="Как у владельца" subtitle={machineName} routeKey="owner-instructions" {onclose}>
  {#if machines.length > 1}
    <Select
      label="Машина"
      options={machines.map((m) => ({ value: m.id, label: m.name }))}
      bind:value={machineId}
    />
  {/if}
  {#if loading}
    <p>Загружаю конфигурацию…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
    <Button onclick={() => void loadOverview()}>Повторить</Button>
  {:else if overview}
    <ul class="summary" data-owner-summary>
      {#each summary as item (item.harness)}
        <li>{item.harness}: {item.text}</li>
      {/each}
      <li>память: {overview.memory_projects.length === 0 ? 'нет проектов' : `${overview.memory_projects.length} проект`}</li>
    </ul>
    {#each overview.files as file (file.kind)}
      {@const meta = OWNER_KIND_META[file.kind]}
      <section data-owner-file={file.kind}>
        <div class="row">
          <strong>{meta?.title ?? file.kind}</strong>
          <span class="meta">{file.present ? `${formatSize(file.size)}${file.mtime ? ` · ${formatOwnerMtime(file.mtime)}` : ''}` : 'отсутствует'}</span>
        </div>
        <p class="note">{meta?.note ?? ''}</p>
        <p class="path">{file.path}</p>
        {#if file.present}
          <Button onclick={() => void toggle(file.kind)}>{expanded === file.kind ? 'Скрыть' : 'Посмотреть'}</Button>
          {#if expanded === file.kind}
            {@const state = contents[file.kind]}
            {#if !state || state.loading}
              <p>Читаю файл…</p>
            {:else if state.error}
              <p class="error" role="alert">{state.error}</p>
            {:else}
              <div class="md" data-owner-content={file.kind}><Html html={engine.render(state.text)} /></div>
              {#if state.truncated}<p class="note">Показан первый кусок файла.</p>{/if}
            {/if}
          {/if}
        {/if}
      </section>
    {/each}
    <section data-owner-memory>
      <div class="row"><strong>Память Claude Code</strong></div>
      {#if overview.memory_projects.length === 0}
        <p class="note">Проектов с памятью нет.</p>
      {:else}
        <Select
          label="Проект"
          options={overview.memory_projects.map((p) => ({ value: p.id, label: p.id }))}
          bind:value={projectId}
        />
        {#if projectId}
          {@const key = 'memory:MEMORY.md'}
          {@const indexState = contents[key]}
          <Button onclick={() => void loadMemory('MEMORY.md')}>Показать MEMORY.md</Button>
          {#if expanded === key || (indexState && !indexState.loading && !indexState.error && expanded?.startsWith('memory:'))}
            {#if !indexState || indexState.loading}
              <p>Читаю файл…</p>
            {:else if indexState.error}
              <p class="error" role="alert">{indexState.error}</p>
            {:else}
              <div class="md" data-owner-content="memory_index"><Html html={engine.render(indexState.text)} /></div>
              {#if indexState.files?.length}
                <ul class="files">
                  {#each indexState.files as item (item.name)}
                    {#if item.name !== 'MEMORY.md'}
                      <li>
                        <button class="link" onclick={() => void loadMemory(item.name)} data-owner-memory-file={item.name}>
                          {item.name} · {formatSize(item.size)}
                        </button>
                        {#if expanded === `memory:${item.name}`}
                          {@const sub = contents[`memory:${item.name}`]}
                          {#if sub && !sub.loading && !sub.error}
                            <div class="md" data-owner-content={item.name}><Html html={engine.render(sub.text)} /></div>
                          {:else if sub && sub.loading}
                            <p>Читаю файл…</p>
                          {/if}
                        {/if}
                      </li>
                    {/if}
                  {/each}
                </ul>
              {/if}
            {/if}
          {/if}
        {/if}
      {/if}
    </section>
  {/if}
  {#if memory}{/if}
</Sheet>

<style>
  .summary { list-style: none; margin: 0 0 var(--s4); padding: var(--s4); background: var(--surface-raised); border-radius: var(--radius-card); display: flex; flex-direction: column; gap: var(--s1); font-size: var(--fs-meta); color: var(--secondary); }
  section { background: var(--surface); border-radius: var(--radius-card); padding: var(--s6); margin-bottom: var(--s4); }
  .row { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: var(--s2); }
  .meta, .note, .path { font-size: var(--fs-meta); color: var(--secondary); }
  .path { overflow-wrap: anywhere; }
  .md { max-height: 40vh; overflow-y: auto; margin-top: var(--s4); overscroll-behavior: contain; }
  .files { list-style: none; margin: var(--s4) 0 0; padding: 0; display: flex; flex-direction: column; gap: var(--s2); }
  .link { background: none; border: 0; padding: 0; color: var(--accent); font: inherit; cursor: pointer; text-align: left; }
  .error { color: var(--danger); }
</style>
