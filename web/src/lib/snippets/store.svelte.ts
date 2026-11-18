import {
  createMessageSnippet,
  deleteMessageSnippet,
  listMessageSnippets,
  patchMessageSnippet,
  reorderMessageSnippets
} from '#lib/api/client.js';
import type { components } from '#lib/api/generated/openapi.js';

export type Snippet = components['schemas']['MessageSnippet'];
export type SnippetScope = 'global' | 'project';

export interface NewSnippet {
  scope: SnippetScope;
  project_key: string;
  title: string;
  body: string;
}

export interface SnippetPatch {
  title: string;
  body: string;
  base_revision: number;
}

/** Заготовки в области списка: глобальные и проекта текущей сессии, с живыми обновлениями. */
export class SnippetsStore {
  items = $state<Snippet[]>([]);
  projectKey = $state('');
  loaded = $state(false);
  error = $state('');

  bind(onSnippet: (fn: (payload: { snippet: unknown; deleted: boolean }) => void) => () => void): void {
    onSnippet((payload) => this.apply(payload));
  }

  setProject(key: string): void {
    if (key === this.projectKey) return;
    this.projectKey = key;
    this.items = [];
    this.loaded = false;
  }

  visible(snippet: Snippet): boolean {
    return snippet.scope === 'global' ||
      (snippet.scope === 'project' && snippet.project_key === this.projectKey);
  }

  /** Проектные заготовки текущей сессии идут перед глобальными. */
  ordered(): Snippet[] {
    return [...this.items].sort((a, b) => {
      const rank = (item: Snippet) => (item.scope === 'project' ? 0 : 1);
      return rank(a) - rank(b) || a.position - b.position || a.title.localeCompare(b.title);
    });
  }

  async load(): Promise<void> {
    try {
      const [global, project] = await Promise.all([
        listMessageSnippets('global'),
        this.projectKey ? listMessageSnippets('project', this.projectKey) : Promise.resolve(null)
      ]);
      this.items = [
        ...(global.data as Snippet[]),
        ...((project?.data as Snippet[] | undefined) ?? [])
      ];
      this.loaded = true;
      this.error = '';
    } catch {
      this.error = 'Не удалось загрузить заготовки. Повторите при восстановлении связи';
    }
  }

  apply(payload: { snippet: unknown; deleted: boolean }): void {
    const snippet = payload.snippet as Snippet | null;
    if (snippet && !this.visible(snippet)) return;
    this.items = payload.deleted || !snippet
      ? this.items.filter((item) => item.id !== snippet?.id)
      : [...this.items.filter((item) => item.id !== snippet.id), snippet];
  }

  async create(input: NewSnippet): Promise<void> {
    const response = await createMessageSnippet(input, crypto.randomUUID());
    this.apply({ snippet: response.data, deleted: false });
  }

  async update(id: string, patch: SnippetPatch): Promise<void> {
    const response = await patchMessageSnippet(id, patch, crypto.randomUUID());
    this.apply({ snippet: response.data, deleted: false });
  }

  async remove(id: string): Promise<void> {
    await deleteMessageSnippet(id, crypto.randomUUID());
    this.items = this.items.filter((item) => item.id !== id);
  }

  async move(scope: SnippetScope, projectKey: string, ids: string[]): Promise<void> {
    const response = await reorderMessageSnippets(
      { scope, project_key: projectKey, ids },
      crypto.randomUUID()
    );
    const fresh = response.data as Snippet[];
    this.items = this.items.map((item) => fresh.find((next) => next.id === item.id) ?? item);
  }
}
