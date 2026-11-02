type UpdateState = 'idle' | 'available' | 'ready' | 'applying' | 'failed';
export let needsUpdate = false;
export function notifyIncompatible(): void {
  needsUpdate = true;
  if (typeof window !== 'undefined') window.dispatchEvent(new Event('canterlot:incompatible'));
}
interface UpdateEnvironment {
  isSafe(): boolean;
  flushDrafts(): Promise<void>;
  hidden(): boolean;
  serviceWorker?: EventTarget;
  reload?(): void;
  /** Ограничивает ожидание активации: без controllerchange всё равно перезагружаемся. */
  activationTimeoutMs?: number;
  schedule?(fn: () => void, ms: number): () => void;
}
type WorkerLike = EventTarget & { state: ServiceWorkerState; postMessage(message: unknown): void };

const ACTIVATION_TIMEOUT = 10_000;
const FLUSH_FAILED = 'Не удалось сохранить черновик. Обновление отложено';
const CHECK_FAILED = 'Не удалось проверить обновление. Проверьте связь и повторите';

export class UpdateController extends EventTarget {
  #state: UpdateState = 'idle';
  #failure = '';
  #changed = false;
  #requested = false;
  #forced = false;
  #flushed = false;
  #applying = false;
  #applyWhenReady = false;
  #destroyed = false;
  #worker?: EventTarget;
  #installing?: WorkerLike;
  #cancelActivation?: () => void;
  #finishPending = false;
  constructor(private reg: ServiceWorkerRegistration, private env: UpdateEnvironment) {
    super();
    this.#worker = env.serviceWorker ?? (typeof navigator !== 'undefined' ? navigator.serviceWorker : undefined);
    this.#worker?.addEventListener('controllerchange', this.#controllerChange);
    reg.addEventListener('updatefound', this.#updateFound);
    this.#observe();
  }
  get state(): UpdateState { return this.#state; }
  get message(): string {
    if (this.#state === 'failed') return this.#failure;
    return this.#state === 'idle' ? '' : 'Доступна новая версия';
  }
  #set(state: UpdateState) { this.#state = state; this.dispatchEvent(new Event('change')); }
  #fail(message: string): void {
    this.#cancelActivation?.(); this.#cancelActivation = undefined;
    this.#applyWhenReady = false;
    this.#failure = message;
    this.#set('failed');
  }
  #announce(state: 'available' | 'ready'): void {
    if (this.#state !== 'applying' && this.#state !== 'failed') this.#set(state);
  }
  #observe() {
    if (this.reg.waiting) { this.#announce('ready'); this.#resumeRequested(); }
    const installing = this.reg.installing as WorkerLike | null;
    if (installing && ['installing', 'installed'].includes(installing.state) && installing !== this.#installing) {
      this.#installing?.removeEventListener('statechange', this.#installed);
      this.#installing = installing;
      this.#announce('available');
      installing.addEventListener('statechange', this.#installed);
    }
  }
  #installed = () => {
    if (this.#installing?.state !== 'installed') return;
    this.#announce('ready');
    this.#resumeRequested();
  };
  #resumeRequested(): void {
    if (!this.#applyWhenReady || this.#applying || this.#destroyed) return;
    queueMicrotask(() => { if (this.#applyWhenReady && !this.#applying && !this.#destroyed) void this.apply(this.#forced); });
  }
  #updateFound = () => this.#observe();
  #controllerChange = () => {
    if (this.#destroyed || !this.#requested) return;
    this.#changed = true;
    this.#finish();
  };
  /** controllerchange может прийти синхронно из postMessage; не теряем перезагрузку за #applying. */
  #finish(): void {
    if (this.#destroyed) return;
    if (this.#applying) { this.#finishPending = true; return; }
    this.#cancelActivation?.(); this.#cancelActivation = undefined;
    void this.#reloadAfterActivation();
  }
  async #reloadAfterActivation(): Promise<void> {
    if (this.#destroyed) return;
    if (!this.#forced && !this.env.hidden() && !this.env.isSafe()) { this.#set('ready'); return; }
    try {
      await this.env.flushDrafts();
    } catch {
      this.#fail(FLUSH_FAILED);
      return;
    }
    if (this.#destroyed) return;
    this.#flushed = true;
    this.#reloadIfAllowed();
  }
  #reloadIfAllowed(): void {
    if (this.#destroyed || !this.#changed || !this.#flushed) return;
    if (!this.#forced && !this.env.hidden() && !this.env.isSafe()) { this.#set('ready'); return; }
    this.#changed = false;
    this.#cancelActivation?.(); this.#cancelActivation = undefined;
    (this.env.reload ?? (() => location.reload()))();
  }
  #armActivation(): void {
    this.#cancelActivation?.();
    const schedule = this.env.schedule ?? ((fn: () => void, ms: number) => { const id = setTimeout(fn, ms); return () => clearTimeout(id); });
    this.#cancelActivation = schedule(() => {
      this.#cancelActivation = undefined;
      if (this.#destroyed || this.#changed) return;
      // Активация не подтвердилась; перезагрузка всё равно выводит на актуальную сборку.
      this.#requested = true;
      this.#changed = true;
      this.#finish();
    }, this.env.activationTimeoutMs ?? ACTIVATION_TIMEOUT);
  }
  #request(waiting: WorkerLike): void {
    this.#applyWhenReady = false;
    this.#requested = true;
    if (this.#state !== 'applying') this.#set('applying');
    this.#armActivation();
    try { waiting.postMessage({ type: 'SKIP_WAITING' }); }
    catch { this.#fail('Не удалось активировать обновление. Обновите страницу'); }
  }
  schemaVersion(version: number): void { if (version !== 1) this.incompatible(); }
  incompatible(): void { if (this.#state === 'idle') this.#set('available'); }
  /** Новую версию увидел SvelteKit (_app/version.json) или хаб: worker может быть ещё не готов. */
  notify(): void {
    if (this.#destroyed || this.#state === 'applying') return;
    this.#announce(this.reg.waiting ? 'ready' : 'available');
    this.#observe();
  }
  async check(): Promise<void> {
    try { await this.reg.update(); } catch { return; }
    await this.#refreshRegistration();
    if (!this.#destroyed) this.#observe();
  }
  async #refreshRegistration(): Promise<void> {
    const container = this.#worker as ServiceWorkerContainer | undefined;
    let current: ServiceWorkerRegistration | undefined;
    try { current = await container?.getRegistration?.(this.reg.scope); } catch { return; }
    if (!current || current === this.reg || this.#destroyed) return;
    this.reg.removeEventListener('updatefound', this.#updateFound);
    this.reg = current;
    current.addEventListener('updatefound', this.#updateFound);
    this.#observe();
  }
  async apply(force = false): Promise<void> {
    if (this.#destroyed || this.#applying) return;
    if (!force && !this.env.hidden() && !this.env.isSafe()) return;
    if (force) this.#forced = true;
    if (this.#state === 'failed') { this.#failure = ''; this.#set('available'); }
    this.#applying = true;
    try {
      await this.#refreshRegistration();
      if (this.#destroyed) return;
      try {
        await this.env.flushDrafts();
      } catch {
        this.#fail(FLUSH_FAILED);
        return;
      }
      if (this.#destroyed) return;
      this.#flushed = true;
      if (this.#changed) { this.#reloadIfAllowed(); return; }
      const waiting = this.#readyWorker();
      if (waiting) { this.#request(waiting); return; }
      if (this.#installing && this.#installing.state !== 'installed') {
        this.#applyWhenReady = true;
        this.#announce('available');
        this.#armActivation();
        return;
      }
      this.#applyWhenReady = true;
      this.#armActivation();
      try {
        await this.reg.update();
      } catch {
        this.#fail(CHECK_FAILED);
        return;
      }
      await this.#refreshRegistration();
      if (this.#destroyed) return;
      const found = this.#readyWorker();
      if (found) { this.#request(found); return; }
      if (this.#installing && this.#installing.state !== 'installed') return;
      // Нового worker нет: он мог уже активироваться. Перезагрузка выводит на актуальную сборку.
      this.#applyWhenReady = false;
      this.#requested = true;
      this.#changed = true;
      this.#set('applying');
      this.#finish();
    } finally {
      this.#applying = false;
      if (this.#finishPending) { this.#finishPending = false; this.#finish(); }
      else if (this.#applyWhenReady && this.#readyWorker()) this.#resumeRequested();
    }
  }
  #readyWorker(): WorkerLike | null {
    if (this.reg.waiting) return this.reg.waiting as WorkerLike;
    return this.#installing?.state === 'installed' ? this.#installing : null;
  }
  destroy(): void {
    this.#destroyed = true;
    this.#cancelActivation?.(); this.#cancelActivation = undefined;
    this.reg.removeEventListener('updatefound', this.#updateFound);
    this.#installing?.removeEventListener('statechange', this.#installed);
    this.#worker?.removeEventListener('controllerchange', this.#controllerChange);
  }
}
