const KEY = 'canterlot.alarms-open';

type Choices = Record<string, boolean>;

function readAll(): Choices {
  try {
    const raw = localStorage.getItem(KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : null;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter((entry): entry is [string, boolean] => typeof entry[1] === 'boolean')
    );
  } catch {
    return {};
  }
}

/** Явный выбор владельца по полосе будильников в этом разговоре: null — ещё не выбирал. */
export function readAlarmsOpen(conversationId: string): boolean | null {
  const value = readAll()[conversationId];
  return typeof value === 'boolean' ? value : null;
}

export function writeAlarmsOpen(conversationId: string, open: boolean): void {
  try {
    localStorage.setItem(KEY, JSON.stringify({ ...readAll(), [conversationId]: open }));
  } catch {
    // Выбор действует до закрытия приложения.
  }
}
