/**
 * Присутствие в чате: пока экран `/c/{id}` открыт и виден, хаб не шлёт пуши об этой
 * сессии. Отметка живёт 45 секунд, поэтому видимая страница повторяет её каждые
 * 20 секунд, а скрытая или уходящая гасит сразу. Страница без касаний, клавиш,
 * прокрутки и движения указателя 15 минут один раз сообщает «не видно» и молчит
 * до первого действия: забытая вкладка не держит сессию бодрствующей.
 */

export const PRESENCE_HEARTBEAT_MS = 20_000;
export const PRESENCE_IDLE_MS = 15 * 60_000;

const ACTIVITY_EVENTS = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart", "scroll"];
const ACTIVITY_LISTENER = { capture: true, passive: true } as const;

export type PresenceSend = (
  conversationId: string,
  visible: boolean,
  keepalive: boolean,
) => void;

/**
 * Отправка без общего HTTP-клиента: `keepalive` переживает уход страницы, а
 * CSRF-заголовок ставится вручную — `sendBeacon` его выставить не умеет.
 */
export function reportPresence(
  conversationId: string,
  visible: boolean,
  keepalive: boolean,
): void {
  try {
    void fetch("/api/v1/presence", {
      method: "POST",
      credentials: "same-origin",
      keepalive,
      headers: {
        "Content-Type": "application/json",
        "X-Canterlot-Client": "1",
      },
      body: JSON.stringify({ conversation_id: conversationId, visible }),
    }).catch(() => undefined);
  } catch {
    // Присутствие — подсказка: без сети пуш просто уйдёт, как раньше.
  }
}

export interface ChatPresenceOptions {
  send?: PresenceSend;
  intervalMs?: number;
  doc?: Document;
  win?: Window;
  now?: () => number;
}

/**
 * Держит отметку одного открытого чата до вызова возвращённой функции.
 * На скрытие и `pagehide` отвечает «не видно», на возврат из bfcache — снова «видно».
 */
export function trackChatPresence(
  conversationId: string,
  options: ChatPresenceOptions = {},
): () => void {
  const doc = options.doc ?? document;
  const win = options.win ?? window;
  const send = options.send ?? reportPresence;
  const intervalMs = options.intervalMs ?? PRESENCE_HEARTBEAT_MS;
  let timer: number | null = null;
  let stopped = false;

  const now = options.now ?? Date.now;
  let lastActivity = now();
  let idle = false;

  const pageVisible = (): boolean => doc.visibilityState === "visible";
  const visible = (): void => {
    if (stopped) return;
    lastActivity = now();
    idle = false;
    send(conversationId, true, false);
    if (timer === null) {
      timer = win.setInterval(() => {
        if (!pageVisible()) return;
        if (now() - lastActivity < PRESENCE_IDLE_MS) {
          send(conversationId, true, false);
        } else if (!idle) {
          idle = true;
          send(conversationId, false, false);
        }
      }, intervalMs);
    }
  };
  const onActivity = (): void => {
    if (stopped) return;
    lastActivity = now();
    if (idle && pageVisible()) {
      idle = false;
      send(conversationId, true, false);
    }
  };
  const hidden = (keepalive: boolean): void => {
    if (timer !== null) {
      win.clearInterval(timer);
      timer = null;
    }
    if (stopped) return;
    send(conversationId, false, keepalive);
  };
  const onVisibility = (): void => {
    if (pageVisible()) visible();
    else hidden(false);
  };
  const onPageHide = (): void => hidden(true);
  const onPageShow = (): void => {
    if (pageVisible()) visible();
  };

  doc.addEventListener("visibilitychange", onVisibility);
  win.addEventListener("pagehide", onPageHide);
  win.addEventListener("pageshow", onPageShow);
  for (const type of ACTIVITY_EVENTS) doc.addEventListener(type, onActivity, ACTIVITY_LISTENER);
  if (pageVisible()) visible();

  return () => {
    if (stopped) return;
    stopped = true;
    if (timer !== null) {
      win.clearInterval(timer);
      timer = null;
    }
    doc.removeEventListener("visibilitychange", onVisibility);
    win.removeEventListener("pagehide", onPageHide);
    win.removeEventListener("pageshow", onPageShow);
    for (const type of ACTIVITY_EVENTS) doc.removeEventListener(type, onActivity, ACTIVITY_LISTENER);
    send(conversationId, false, true);
  };
}
