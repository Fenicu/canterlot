import { SvelteMap } from 'svelte/reactivity';
import type { SecretRequestView } from '#lib/requests/secretRequest.js';

/**
 * Секретные запросы разговора: список с хаба и живые события `secret_request_changed`.
 * Якорь в ленте запоминается один раз при создании и не переезжает при смене состояния.
 */
export class SecretRequestsModel {
  readonly items = new SvelteMap<string, SecretRequestView>();
  readonly conversationId: string;
  readonly #anchors = new SvelteMap<string, string>();

  constructor(conversationId: string) {
    this.conversationId = conversationId;
  }

  /** Снимок списка: применяются только версии не старее уже известной. */
  ingest(items: SecretRequestView[]): SecretRequestView[] {
    const applied: SecretRequestView[] = [];
    for (const item of items) {
      if (this.#store(item)) applied.push(item);
    }
    return applied;
  }

  /** Ответ или событие: старое не затирает новое состояние карточки. */
  apply(view: SecretRequestView, anchor?: string): boolean {
    if (!this.#store(view)) return false;
    if (anchor && !this.#anchors.has(view.id)) this.#anchors.set(view.id, anchor);
    return true;
  }

  #store(view: SecretRequestView): boolean {
    const current = this.items.get(view.id);
    if (current && current.revision > view.revision) return false;
    this.items.set(view.id, view);
    return true;
  }

  anchorOf(id: string): string | undefined {
    return this.#anchors.get(id);
  }

  setAnchor(id: string, anchor: string): void {
    if (!this.#anchors.has(id)) this.#anchors.set(id, anchor);
  }

  get(id: string): SecretRequestView | undefined {
    return this.items.get(id);
  }

  clear(): void {
    this.items.clear();
    this.#anchors.clear();
  }
}
