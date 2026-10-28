interface QueuedAnnouncement {
  message: string;
  politeness: 'polite' | 'assertive';
}

let host: HTMLElement | undefined;
const recent = new Map<string, number>();
const queue: QueuedAnnouncement[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;

function pruneRecent(now: number): void {
  for (const [text, at] of recent) {
    if (now - at >= 2000) recent.delete(text);
  }
}

function processQueue(): void {
  timer = undefined;
  if (!host || queue.length === 0) return;
  const item = queue.shift()!;
  recent.set(item.message, Date.now());
  host.setAttribute('aria-live', item.politeness);
  host.textContent = '';
  host.textContent = item.message;
  if (queue.length > 0) {
    timer = setTimeout(processQueue, 50);
  }
}

export function mountAnnouncements(node: HTMLElement): { destroy: () => void } {
  host = node;
  return {
    destroy: () => {
      host = undefined;
      recent.clear();
      queue.length = 0;
      clearTimeout(timer);
      timer = undefined;
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
  if (!timer) {
    timer = setTimeout(processQueue, 0);
  }
}
