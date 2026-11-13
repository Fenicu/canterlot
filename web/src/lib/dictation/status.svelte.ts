/** Общее состояние диктовки для поля ввода и настроек: доступность и «нет микрофона». */
import { getDictationStatus } from "./api.js";
import { DictationAvailability, type DictationStatusView } from "./availability.js";

const availability = new DictationAvailability({
  fetch: async () => (await getDictationStatus()).data,
  now: () => Date.now(),
  schedule: (fn, ms) => {
    const timer = setTimeout(fn, ms);
    return () => clearTimeout(timer);
  },
});

let snapshot = $state<{ status: DictationStatusView | null; missing: boolean }>({
  status: null,
  missing: false,
});

async function refresh(force = false): Promise<void> {
  await availability.ensure(force);
  snapshot = { ...snapshot, status: availability.status };
}

async function recheckMicrophone(): Promise<void> {
  try {
    const devices = await navigator.mediaDevices?.enumerateDevices?.();
    if (devices?.some((device) => device.kind === "audioinput")) snapshot.missing = false;
  } catch {
    // Список устройств закрыт до разрешения: значит, микрофона так и не нашлось.
  }
}

export const dictationStatus = {
  get status(): DictationStatusView | null {
    return snapshot.status;
  },
  get available(): boolean {
    return snapshot.status?.available === true;
  },
  get missing(): boolean {
    return snapshot.missing;
  },
  /** Возврат в приложение: не чаще TTL, ошибки откладывают следующий запрос. */
  refresh,
  /** Служба отказала прямо во время записи: следующий запрос идёт сразу. */
  noteUnavailable(): void {
    availability.invalidate();
    void refresh(true);
  },
  hideForMissingMicrophone(): void {
    snapshot.missing = true;
  },
  recheckMicrophone,
};

if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void refresh();
  });
}

if (typeof navigator !== "undefined") {
  navigator.mediaDevices?.addEventListener?.("devicechange", () => {
    if (snapshot.missing) void recheckMicrophone();
  });
}
