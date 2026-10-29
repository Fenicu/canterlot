import type { DraftStore } from "#lib/drafts/store.js";

export interface DraftBindingOptions {
  drafts: DraftStore;
  conversationId: string;
  onRestore?: (text: string) => void;
  onChange?: (text: string) => void;
}

/**
 * Привязка черновика к полю ввода:
 * - Загружает черновик при монтировании (восстановленный текст никогда не отправляется автоматически).
 * - Сохраняет изменения с дебаунсом 250 мс через DraftStore.
 * - Сбрасывает на диск при потере фокуса (blur).
 */
export function bindDraft(
  node: HTMLTextAreaElement,
  options: DraftBindingOptions,
): { destroy(): void } {
  let unmounted = false;

  void options.drafts.load(options.conversationId).then((draft) => {
    if (unmounted) return;
    if (draft && draft.text && !node.value) {
      node.value = draft.text;
      options.onRestore?.(draft.text);
    }
  });

  const onInput = () => {
    const text = node.value;
    options.drafts.set(options.conversationId, { text });
    options.onChange?.(text);
  };

  node.addEventListener("input", onInput);
  const unwatch = options.drafts.watchComposer(node);

  return {
    destroy() {
      unmounted = true;
      node.removeEventListener("input", onInput);
      unwatch();
      void options.drafts.flush();
    },
  };
}
