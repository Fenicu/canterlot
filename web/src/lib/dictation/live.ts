/** Живая диктовка в браузере: сокет хаба и микрофон, собранные в одну сессию. */
import { APP_VERSION } from "#lib/stores/appVersion.js";
import { MicrophoneCapture } from "./capture.js";
import {
  DictationSession,
  type DictationConnectHandlers,
  type DictationSocket,
  type DictationView,
} from "./session.js";

export const DICTATION_PATH = "/api/v1/dictation";

/** Тот же origin и та же cookie, что у основного сокета приложения. */
export function openDictationSocket(handlers: DictationConnectHandlers): DictationSocket {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  const socket = new WebSocket(`${scheme}//${location.host}${DICTATION_PATH}`);
  socket.binaryType = "arraybuffer";
  socket.addEventListener("open", () => handlers.open());
  socket.addEventListener("message", (event: MessageEvent) => {
    if (typeof event.data === "string") handlers.message(event.data);
  });
  socket.addEventListener("close", (event: CloseEvent) => handlers.close(event.code));
  return {
    send(data: string | ArrayBuffer) {
      if (socket.readyState === WebSocket.OPEN) socket.send(data);
    },
    close() {
      socket.close();
    },
    get bufferedAmount() {
      return socket.bufferedAmount;
    },
  };
}

export function createDictationSession(handlers: {
  onview(view: DictationView): void;
  oninsert(text: string, notice: string): void;
  onunavailable?(): void;
}): DictationSession {
  return new DictationSession({
    connect: openDictationSocket,
    startCapture: (hooks) => new MicrophoneCapture(hooks),
    schedule: (fn, ms) => {
      const timer = setTimeout(fn, ms);
      return () => clearTimeout(timer);
    },
    now: () => Date.now(),
    // Версию приложения знает сборка: хаб пишет её в журнал.
    client: `pwa/${APP_VERSION}`,
    ...handlers,
  });
}
