import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

const store = {
  loaded: true,
  error: '',
  projectKey: '',
  ordered: () => [],
  setProject: vi.fn((key: string) => { store.projectKey = key; }),
  load: vi.fn(async () => undefined),
  create: vi.fn(async () => undefined)
};
// Первой в списке стоит сессия чужого проекта: раньше её папка молча становилась папкой заготовки.
const sessions = new Map([
  ['s1', { id: 's1', workdir: '/srv/notes' }],
  ['s2', { id: 's2', workdir: '/srv/lighthouse' }],
  ['s3', { id: 's3', workdir: '/srv/lighthouse' }],
  ['s4', { id: 's4', workdir: '' }]
]);
vi.mock('#lib/app/runtime.js', () => ({ getRuntime: () => ({ snippets: store, list: { items: sessions } }) }));

const { default: SnippetsSettings } = await import('#lib/screens/settings/SnippetsSettings.svelte');

function fill(node: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  node.value = value;
  node.dispatchEvent(new Event(node instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
  flushSync();
}

describe('settings/project snippets', () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    document.body.textContent = '';
  });

  it('saves a project snippet into the folder the owner picked and names that folder', async () => {
    instance = mount(SnippetsSettings, { target: document.body });
    flushSync();
    const field = <T extends Element>(label: string) => document.querySelector<T>(`[aria-label="${label}"]`)!;

    fill(field<HTMLSelectElement>('Область заготовки'), 'project');
    const folder = field<HTMLSelectElement>('Папка проекта');
    // Папки сессий без повторов; выбор виден на экране, а не делается за владельца молча.
    expect([...folder.options].map((option) => option.value)).toEqual(['/srv/lighthouse', '/srv/notes']);

    fill(folder, '/srv/lighthouse');
    fill(field<HTMLInputElement>('Название заготовки'), 'Проверка');
    fill(field<HTMLTextAreaElement>('Текст заготовки'), 'Прогони быструю проверку');
    [...document.querySelectorAll('button')].find((node) => node.textContent?.trim() === 'Добавить заготовку')!.click();

    await vi.waitFor(() => expect(store.create).toHaveBeenCalledTimes(1));
    expect(store.create).toHaveBeenCalledWith({ scope: 'project', project_key: '/srv/lighthouse', title: 'Проверка', body: 'Прогони быструю проверку' });
    // Список ниже показывает заготовки той же папки.
    expect(store.setProject).toHaveBeenLastCalledWith('/srv/lighthouse');
  });
});
