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

  /** Снимок списка: порядок в ленте задаёт `created_at`, а не порядок ответа. */
  ingest(items: SecretRequestView[]): void {
    for (const item of items) this.items.set(item.id, item);
  }

  apply(view: SecretRequestView, anchor?: string): void {
    this.items.set(view.id, view);
    if (anchor && !this.#anchors.has(view.id)) this.#anchors.set(view.id, anchor);
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
