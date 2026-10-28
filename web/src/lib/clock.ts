/**
 * Синхронизация времени с сервером по заголовку X-Server-Time-Ms.
 * Формула NTP: offset = serverMs - (sentAt + receivedAt) / 2.
 * Выбирается замер с наименьшим round-trip time (RTT).
 */
class ClockSync {
  #offset = 0;
  #minRtt = Infinity;

  get offset(): number {
    return this.#offset;
  }

  get rtt(): number {
    return this.#minRtt;
  }

  update(serverTimeMs: number, sentAt: number, receivedAt: number): void {
    if (typeof serverTimeMs !== 'number' || isNaN(serverTimeMs)) return;
    const rtt = Math.max(0, receivedAt - sentAt);
    if (rtt < this.#minRtt) {
      this.#minRtt = rtt;
      const midpoint = sentAt + rtt / 2;
      this.#offset = Math.round(serverTimeMs - midpoint);
    }
  }

  serverNow(): number {
    return Date.now() + this.#offset;
  }

  reset(): void {
    this.#offset = 0;
    this.#minRtt = Infinity;
  }
}

export const clock = new ClockSync();
