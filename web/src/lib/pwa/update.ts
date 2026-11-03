type UpdateState = 'idle' | 'available' | 'applying' | 'failed';
export let needsUpdate = false;
export function notifyIncompatible(): void {
  needsUpdate = true;
  if (typeof window !== 'undefined') window.dispatchEvent(new Event('canterlot:incompatible'));
}
interface UpdateEnvironment {
  isSafe(): boolean;
  /** Открыта правка, текст которой живёт только на экране: даже скрытую страницу не перезагружаем. */
  editing?(): boolean;
  flushDrafts(): Promise<void>;
  hidden(): boolean;
  reload?(): void;
  /** Версия текущей сборки; сравнивается с _app/version.json. */
  version?: string;
  fetch?(input: string, init?: RequestInit): Promise<Response>;
}

const FLUSH_FAILED = 'Не удалось сохранить черновик. Обновление отложено';

export class UpdateController extends EventTarget {
  #state: UpdateState = 'idle';
  #failure = '';
  #applying = false;
  #destroyed = false;
  constructor(private env: UpdateEnvironment) {
    super();
  }
  get state(): UpdateState { return this.#state; }
  get message(): string {
    if (this.#state === 'failed') return this.#failure;
    return this.#state === 'idle' ? '' : 'Доступна новая версия';
  }
  #set(state: UpdateState) { this.#state = state; this.dispatchEvent(new Event('change')); }
  #fail(message: string): void { this.#failure = message; this.#set('failed'); }
  /** Новая версия есть по _app/version.json или из-за несовместимости протокола. */
  notify(): void {
    if (this.#destroyed || this.#state === 'applying') return;
    this.#failure = '';
    this.#set('available');
  }
  incompatible(): void { if (this.#state === 'idle') this.notify(); }
  /** Сверяет _app/version.json без HTTP-кэша. Сетевые ошибки ничего не меняют. */
  async check(): Promise<void> {
    const fetcher = this.env.fetch ?? (typeof fetch === 'undefined' ? undefined : fetch);
    if (this.#destroyed || !this.env.version || !fetcher) return;
    let version: string | undefined;
    try {
      const response = await fetcher('/_app/version.json', {
        cache: 'no-store',
        headers: { 'cache-control': 'no-cache' }
      });
      if (!response.ok) return;
      const data = await response.json() as { version?: unknown };
      version = typeof data.version === 'string' ? data.version : undefined;
    } catch {
      return;
    }
    if (!this.#destroyed && version !== undefined && version !== this.env.version) this.notify();
  }
  async apply(force = false): Promise<void> {
    if (this.#destroyed || this.#applying) return;
    if (!force && (this.env.editing?.() || (!this.env.hidden() && !this.env.isSafe()))) return;
    this.#applying = true;
    try {
      this.#failure = '';
      this.#set('applying');
      try {
        await this.env.flushDrafts();
      } catch {
        this.#fail(FLUSH_FAILED);
        return;
      }
      if (this.#destroyed) return;
      if (this.env.reload) { this.env.reload(); return; }
      // Полная перезагрузка: оболочку больше не отдаёт service worker, а HTML
      // запрашиваем без кэша перед переходом.
      try { await fetch(location.href, { cache: 'no-store', credentials: 'same-origin', signal: AbortSignal.timeout(3000) }); } catch { /* сеть решает переход ниже */ }
      if (!this.#destroyed) location.reload();
    } finally {
      this.#applying = false;
    }
  }
  destroy(): void { this.#destroyed = true; }
}
