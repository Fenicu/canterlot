import type { ShareItem } from '$lib/pwa/share';

export async function applyShare(item: ShareItem, model: { text: string; attach(files: File[]): void; errorMessage: string | null }, save: (text: string) => Promise<void>, remove: () => Promise<void>): Promise<void> {
  model.attach(item.files);
  model.text = [model.text, item.title, item.text, item.url].filter(Boolean).join('\n');
  await save(model.text);
  await remove();
}
