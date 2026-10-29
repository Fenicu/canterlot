interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}
export class InstallController extends EventTarget {
  #event: InstallPrompt | null = null;
  #used = false;
  constructor(private target: EventTarget = window) {
    super();
    target.addEventListener('beforeinstallprompt', this.#capture);
    target.addEventListener('appinstalled', this.#installed);
  }
  #capture = (event: Event) => {
    event.preventDefault();
    if (this.#used || this.#event) return;
    this.#event = event as InstallPrompt;
    this.dispatchEvent(new Event('change'));
  };
  #installed = () => { this.#used = true; this.#event = null; this.dispatchEvent(new Event('change')); };
  get canInstall(): boolean { return !!this.#event && !this.#used; }
  async prompt(): Promise<'accepted' | 'dismissed'> {
    const event = this.#event;
    if (!event || this.#used) return 'dismissed';
    this.#used = true; this.#event = null; this.dispatchEvent(new Event('change'));
    await event.prompt();
    return (await event.userChoice).outcome;
  }
  destroy(): void { this.target.removeEventListener('beforeinstallprompt', this.#capture); this.target.removeEventListener('appinstalled', this.#installed); this.#event = null; }
}
