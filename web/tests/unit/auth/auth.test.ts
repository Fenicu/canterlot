import { describe, expect, it } from "vitest";
import { cascadeOf, cascadeText, placeText, plural, seenText } from "#lib/auth/sessions.js";
import { lockText } from "#lib/auth/signin.js";
import { cameraErrorText, qrSecret } from "#lib/auth/scan.js";
import { fromBase64url, toBase64url } from "#lib/auth/webauthn.js";
import { activityLine } from "#lib/screens/sessions/activityText.js";
import type { OwnerSession } from "#lib/auth/api.js";

const session = (id: string, approved: string[] = [], name: string | null = null): OwnerSession => ({
  id, approved, name, label: `Устройство ${id}`, method: "qr", created_at: "2026-11-12T10:00:00Z",
  last_seen_at: "2026-11-12T10:00:00Z", current: false,
});

describe("auth/sessions", () => {
  it("cascade_follows_qr_approvals_transitively_without_loops", () => {
    const list = [session("a", ["b"]), session("b", ["c", "a"]), session("c"), session("d")];
    expect(cascadeOf("a", list).map((s) => s.id)).toEqual(["b", "c"]);
    expect(cascadeOf("d", list)).toEqual([]);
  });

  it("cascade_text_names_the_sessions_with_russian_plurals", () => {
    expect(cascadeText([])).toBe("");
    expect(cascadeText([session("b", [], "Пиксель")])).toBe("Также будет завершён 1 сеанс, вход которого подтвердило это устройство: Пиксель");
    expect(cascadeText([session("b"), session("c")])).toBe("Также будут завершены 2 сеанса, вход которых подтвердило это устройство: Устройство b, Устройство c");
    expect(plural(5, "сеанс", "сеанса", "сеансов")).toBe("сеансов");
    expect(plural(21, "сеанс", "сеанса", "сеансов")).toBe("сеанс");
    expect(plural(12, "сеанс", "сеанса", "сеансов")).toBe("сеансов");
  });

  it("place_reads_local_network_city_and_country", () => {
    expect(placeText(null)).toBeNull();
    expect(placeText({ local: true })).toBe("локальная сеть");
    expect(placeText({ city: "Москва", country: "Россия", local: false })).toBe("Москва, Россия");
    expect(placeText({ local: false })).toBeNull();
  });

  it("seen_text_is_relative", () => {
    const now = Date.parse("2026-11-12T10:30:00Z");
    expect(seenText("2026-11-12T10:29:40Z", now)).toBe("был(а) в сети только что");
    expect(seenText("2026-11-12T10:25:00Z", now)).toBe("был(а) в сети 5 мин назад");
    expect(seenText("2026-11-12T07:30:00Z", now)).toBe("был(а) в сети 3 ч назад");
  });
});

describe("auth/signin", () => {
  it("lock_text_rounds_minutes_up", () => {
    expect(lockText(240)).toBe("Слишком много попыток. Повторите через 4 мин");
    expect(lockText(61)).toBe("Слишком много попыток. Повторите через 2 мин");
    expect(lockText(5)).toBe("Слишком много попыток. Повторите через 1 мин");
  });
});

describe("auth/scan", () => {
  const origin = "https://hub.example";
  const secret = "A".repeat(43);
  it("accepts_only_same_origin_qr_links", () => {
    expect(qrSecret(`${origin}/qr#${secret}`, origin)).toBe(secret);
    expect(qrSecret(`https://evil.example/qr#${secret}`, origin)).toBeNull();
    expect(qrSecret(`${origin}/recover#${secret}`, origin)).toBeNull();
    expect(qrSecret(`${origin}/qr?x=1#${secret}`, origin)).toBeNull();
    expect(qrSecret(`${origin}/qr#`, origin)).toBeNull();
    expect(qrSecret("просто текст", origin)).toBeNull();
  });
  it("camera_errors_in_words", () => {
    expect(cameraErrorText(new DOMException("x", "NotAllowedError"))).toMatch(/Нет доступа к камере/);
    expect(cameraErrorText(new DOMException("x", "NotFoundError"))).toBe("Камера не найдена");
  });
});

describe("auth/webauthn", () => {
  it("base64url_round_trips_binary", () => {
    const bytes = new Uint8Array([0, 255, 62, 63, 250, 1, 2]);
    const text = toBase64url(bytes);
    expect(text).not.toMatch(/[+/=]/);
    expect(new Uint8Array(fromBase64url(text))).toEqual(bytes);
  });
});

describe("delegated row duration", () => {
  it("running_tool_line_is_marked_timed", () => {
    const now = Date.parse("2026-11-12T10:04:00Z");
    const line = activityLine({ state: "working", since: "2026-11-12T10:00:00Z", evidence: { phase: "running_tool", tool_name: "Bash", summary: "cargo nextest", phase_since: "2026-11-12T10:00:00Z" } }, { now });
    expect(line.text).toBe("Выполняет cargo nextest · 4 мин");
    expect(line.timed).toBe(true);
    expect(activityLine({ state: "working", evidence: { phase: "generating" } }, { now }).timed).toBeFalsy();
  });
});
