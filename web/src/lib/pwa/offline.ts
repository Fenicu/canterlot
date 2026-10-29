export class Staleness extends EventTarget {
  online = typeof navigator === 'undefined' || navigator.onLine;
  lastSyncAt: number | null = null;
  synced(at: number = Date.now()): void { this.lastSyncAt = at; this.dispatchEvent(new Event('change')); }
  setOnline(online: boolean): void { this.online = online; this.dispatchEvent(new Event('change')); }
  label(now: number): string {
    if (this.online) return '';
    if (this.lastSyncAt === null) return 'Нет связи · данные ещё не загружены';
    const time = new Intl.DateTimeFormat('ru', { hour: '2-digit', minute: '2-digit' }).format(Math.min(now, this.lastSyncAt));
    return `Нет связи · данные от ${time}`;
  }
  reset(): void { this.lastSyncAt = null; this.dispatchEvent(new Event('change')); }
}
export const staleness = new Staleness();
