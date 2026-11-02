/** Время последней успешной синхронизации списка для экрана настроек. */
class SyncState {
  lastSyncAt: number | null = null;
  synced(at: number): void { this.lastSyncAt = at; }
  reset(): void { this.lastSyncAt = null; }
}
export const staleness = new SyncState();
