interface QueuedAnnouncement {
  message: string;
  politeness: 'polite' | 'assertive';
}

let host: HTMLElement | undefined;
const recent = new Map<string, number>();
const queue: QueuedAnnouncement[] = [];
const removalTimers = new Set<ReturnType<typeof setTimeout>>();
let queueTimer: ReturnType<typeof setTimeout> | undefined;

function pruneRecent(now: number): void {
  for (const [text, at] of recent) {
    if (now - at >= 2000) recent.delete(text);
  }
}

function processQueue(): void {
  queueTimer = undefined;
  if (!host) {
    queue.length = 0;
    return;
  }
  const now = Date.now();
  while (queue.length > 0) {
    const item = queue.shift()!;
    recent.set(item.message, now);
    const child = document.createElement('div');
    child.textContent = item.message;
    if (item.politeness === 'assertive') {
      child.setAttribute('aria-live', 'assertive');
    }
    host.appendChild(child);

    const removeTimer = setTimeout(() => {
      child.remove();
      removalTimers.delete(removeTimer);
    }, 5000);
    removalTimers.add(removeTimer);
  }
}

export function mountAnnouncements(node: HTMLElement): { destroy: () => void } {
  host = node;
  host.setAttribute('aria-live', 'polite');
  return {
    destroy: () => {
      if (queueTimer !== undefined) {
        clearTimeout(queueTimer);
        queueTimer = undefined;
      }
      for (const t of removalTimers) {
        clearTimeout(t);
      }
      removalTimers.clear();
      if (host) {
        while (host.firstChild) {
          host.removeChild(host.firstChild);
        }
        host = undefined;
      }
      recent.clear();
      queue.length = 0;
    }
  };
}

export function announce(message: string, politeness: 'polite' | 'assertive' = 'polite'): void {
  if (!host) return;
  const now = Date.now();
  pruneRecent(now);
  if (recent.has(message)) return;
  if (queue.some(q => q.message === message)) return;
  queue.push({ message, politeness });
  if (!queueTimer) {
    queueTimer = setTimeout(processQueue, 0);
  }
}
