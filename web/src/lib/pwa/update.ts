type UpdateState = 'idle' | 'available' | 'ready' | 'applying';
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
}

export class UpdateController extends EventTarget {
  #state: UpdateState = 'idle';
  #changed = false;
  #requested = false;
  #flushed = false;
  #applying = false;
  #applyWhenReady = false;
  #destroyed = false;
  #worker?: EventTarget;
  #installing?: ServiceWorker;
  constructor(private reg: ServiceWorkerRegistration, private env: UpdateEnvironment) {
    super();
    this.#worker = env.serviceWorker ?? (typeof navigator !== 'undefined' ? navigator.serviceWorker : undefined);
    this.#worker?.addEventListener('controllerchange', this.#controllerChange);
    reg.addEventListener('updatefound', this.#updateFound);
    this.#observe();
  }
  get state(): UpdateState { return this.#state; }
  get message(): string { return this.#state === 'idle' ? '' : 'Доступна новая версия'; }
  #set(state: UpdateState) { this.#state = state; this.dispatchEvent(new Event('change')); }
  #observe() {
    if (this.reg.waiting) { this.#set('ready'); this.#resumeRequested(); }
    const installing = this.reg.installing;
    if (installing && ['installing', 'installed'].includes(installing.state) && installing !== this.#installing) {
      this.#installing?.removeEventListener('statechange', this.#installed);
      this.#installing = installing;
      this.#set('available');
      installing.addEventListener('statechange', this.#installed);
    }
  }
  #installed = () => { if (this.#installing?.state === 'installed') { this.#set('ready'); this.#resumeRequested(); } };
  #resumeRequested(): void {
    if (!this.#applyWhenReady || this.#applying || this.#destroyed) return;
    queueMicrotask(() => { void this.apply().catch(() => { this.#applyWhenReady = false; this.#set('ready'); }); });
  }
  #updateFound = () => this.#observe();
  #controllerChange = () => {
    if (!this.#requested) return;
    this.#changed = true;
    if (!this.env.hidden() && !this.env.isSafe()) { this.#set('ready'); return; }
    void this.apply().catch(() => this.#set('ready'));
  };
  #reloadIfSafe() {
    if (this.#destroyed || !this.#changed || !this.#flushed) return;
    if (!this.env.hidden() && !this.env.isSafe()) { this.#set('ready'); return; }
    this.#changed = false;
    (this.env.reload ?? (() => location.reload()))();
  }
  schemaVersion(version: number): void { if (version !== 1) this.incompatible(); }
  incompatible(): void { if (this.#state === 'idle') this.#set('available'); }
  async check(): Promise<void> {
    await this.reg.update();
    await this.#refreshRegistration();
    if (!this.#destroyed) this.#observe();
  }
  async #refreshRegistration(): Promise<void> {
    const container = this.#worker as ServiceWorkerContainer | undefined;
    const current = await container?.getRegistration?.(this.reg.scope);
    if (!current || current === this.reg || this.#destroyed) return;
    this.reg.removeEventListener('updatefound', this.#updateFound);
    this.reg = current;
    current.addEventListener('updatefound', this.#updateFound);
    this.#observe();
  }
  async apply(): Promise<void> {
    if (this.#destroyed || this.#applying || (!this.env.hidden() && !this.env.isSafe())) return;
    this.#applying = true;
    try {
      await this.#refreshRegistration();
      await this.env.flushDrafts();
      if (this.#destroyed || (!this.env.hidden() && !this.env.isSafe())) return;
      this.#flushed = true;
      if (this.#changed) { this.#reloadIfSafe(); return; }
      const waiting = this.reg.waiting ?? (this.#installing?.state === 'installed' ? this.#installing : null);
      if (!waiting) { this.#applyWhenReady = true; await this.check(); return; }
      this.#applyWhenReady = false;
      this.#requested = true;
      this.#set('applying');
      waiting.postMessage({ type: 'SKIP_WAITING' });
    } finally {
      this.#applying = false;
      if (this.reg.waiting || this.#installing?.state === 'installed') this.#resumeRequested();
    }
  }
  destroy(): void {
    this.#destroyed = true;
    this.reg.removeEventListener('updatefound', this.#updateFound);
    this.#installing?.removeEventListener('statechange', this.#installed);
    this.#worker?.removeEventListener('controllerchange', this.#controllerChange);
  }
}
