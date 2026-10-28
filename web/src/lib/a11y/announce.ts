let host: HTMLElement | undefined;
const recent = new Map<string, number>();
let pending: ReturnType<typeof setTimeout> | undefined;
export function mountAnnouncements(node: HTMLElement): { destroy: () => void } {
  host = node;
  return { destroy: () => { host = undefined; recent.clear(); clearTimeout(pending); } };
}
export function announce(message: string, politeness: 'polite' | 'assertive' = 'polite'): void {
  if (!host) return;
  const now = Date.now();
  for (const [text, at] of recent) if (now - at >= 2000) recent.delete(text);
  if (recent.has(message)) return;
  recent.set(message, now);
  clearTimeout(pending);
  host.setAttribute('aria-live', politeness);
  host.textContent = '';
  pending = setTimeout(() => { if (host) host.textContent = message; }, 0);
}
