import { describe, expect, it } from "vitest";
import { retryText, type RetryFacts } from "#lib/format/labels.js";

const T0 = Date.parse("2026-11-19T10:00:00.000Z");
const base: RetryFacts = {
  attempt: 2,
  max_retries: 10,
  delay_ms: 8000,
  http_status: 529,
  category: "overloaded",
  no_response_waited_ms: null,
};

describe("retryText", () => {
  it("рисует попытку, причину со статусом и обратный отсчёт", () => {
    expect(retryText({ retry: base, summary: "retry 2/10", since: T0, now: T0 })).toBe(
      "Повторяет запрос к модели: попытка 2 из 10 · перегрузка (529) · через 8 с",
    );
  });

  it("отсчёт идёт от времени события и пропадает на нуле", () => {
    const at = (ms: number) => retryText({ retry: base, summary: null, since: T0, now: T0 + ms });
    expect(at(1) ).toContain("через 8 с");
    expect(at(2500)).toContain("через 6 с");
    expect(at(7999)).toContain("через 1 с");
    expect(at(8000)).toBe("Повторяет запрос к модели: попытка 2 из 10 · перегрузка (529)");
    expect(at(60_000)).not.toContain("через");
  });

  it("длинный отсчёт идёт минутами", () => {
    const text = retryText({ retry: { ...base, delay_ms: 65_000 }, summary: null, since: T0, now: T0 });
    expect(text).toContain("через 1 мин 05 с");
  });

  it.each([
    ["overloaded", "перегрузка"],
    ["rate_limit", "лимит"],
    ["server_error", "ошибка сервера"],
    ["authentication_failed", "ошибка доступа"],
    ["oauth_org_not_allowed", "ошибка доступа"],
    ["account_on_hold", "ошибка доступа"],
    ["billing_error", "ошибка доступа"],
    ["cloud_credential_error", "ошибка доступа"],
    ["invalid_request", "ошибка запроса"],
    ["model_not_found", "ошибка запроса"],
    ["max_output_tokens", "ошибка запроса"],
    ["unknown", "ошибка"],
    ["something_new", "ошибка"],
  ])("категория %s называется «%s»", (category, label) => {
    const text = retryText({
      retry: { ...base, category, http_status: null, delay_ms: 0 },
      summary: null,
      since: T0,
      now: T0,
    });
    expect(text).toBe(`Повторяет запрос к модели: попытка 2 из 10 · ${label}`);
  });

  it("без статуса скобок нет, без потолка нет «из»", () => {
    const text = retryText({
      retry: { ...base, http_status: null, max_retries: null, delay_ms: 0 },
      summary: null,
      since: T0,
      now: T0,
    });
    expect(text).toBe("Повторяет запрос к модели: попытка 2 · перегрузка");
  });

  it("нет ответа: ожидание и попытка", () => {
    const text = retryText({
      retry: { ...base, http_status: null, category: "unknown", no_response_waited_ms: 185_000, delay_ms: 4000 },
      summary: null,
      since: T0,
      now: T0,
    });
    expect(text).toBe("Нет ответа 3 мин 05 с · попытка 2 из 10 · через 4 с");
  });

  it("нет ответа короче минуты", () => {
    const text = retryText({
      retry: { ...base, no_response_waited_ms: 45_000, delay_ms: 0 },
      summary: null,
      since: T0,
      now: T0,
    });
    expect(text).toBe("Нет ответа 45 с · попытка 2 из 10");
  });

  it("без времени события отсчёта нет", () => {
    expect(retryText({ retry: base, summary: null, since: null, now: T0 })).not.toContain("через");
  });

  it("старая строка: retry N и retry N/M", () => {
    expect(retryText({ retry: null, summary: "retry 3", since: null, now: T0 })).toBe(
      "Повторяет запрос к модели, попытка 3",
    );
    expect(retryText({ retry: null, summary: "retry 2/10", since: null, now: T0 })).toBe(
      "Повторяет запрос к модели, попытка 2 из 10",
    );
  });

  it("не повтор: null", () => {
    expect(retryText({ retry: null, summary: "Субагент general: done", since: null, now: T0 })).toBeNull();
    expect(retryText({ retry: undefined, summary: undefined, since: null, now: T0 })).toBeNull();
  });

  it("мусорный payload не рисуется", () => {
    expect(retryText({ retry: { attempt: "x" } as never, summary: null, since: null, now: T0 })).toBeNull();
  });
});
