import { describe, expect, it } from "vitest";
import { SettingsFormSync, type SettingsFormValues } from "#lib/screens/chat/settingsForm.js";

const hub = (over: Partial<SettingsFormValues> = {}): SettingsFormValues => ({
  model: "opus",
  effort: "medium",
  permission: "owner",
  preset: "clean",
  crunchEnabled: false,
  resumeText: "Продолжай",
  ...over,
});

describe("лист настроек сессии: поля формы и хаб", () => {
  it("открытие листа берёт значения хаба", () => {
    const form = new SettingsFormSync();
    const next = form.sync({ rev: 3, crunchKey: "a", source: hub({ model: "sonnet" }), current: hub() });
    expect(next?.model).toBe("sonnet");
  });

  it("снимок с той же ревизией не сбрасывает выбор владельца", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    current.model = "haiku";
    current.resumeText = "Мой текст";
    // Переподключение: снимок переприсвоил настройки и кранч теми же значениями.
    expect(form.sync({ rev: 3, crunchKey: "a", source: hub(), current })).toBeNull();
    expect(current.model).toBe("haiku");
  });

  it("новая ревизия обновляет нетронутые поля и не трогает изменённые владельцем", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    current.model = "haiku";
    const next = form.sync({ rev: 4, crunchKey: "a", source: hub({ model: "sonnet", effort: "high", permission: "ask" }), current })!;
    expect(next.model).toBe("haiku");
    expect(next.effort).toBe("high");
    expect(next.permission).toBe("ask");
  });

  it("смена кранча с другого устройства не затирает набираемый текст продолжения", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    current.resumeText = "Набираю";
    const next = form.sync({ rev: 3, crunchKey: "b", source: hub({ crunchEnabled: true, resumeText: "Чужой" }), current })!;
    expect(next.resumeText).toBe("Набираю");
    expect(next.crunchEnabled).toBe(true);
    // Кранч не меняет поля настроек, даже нетронутые.
    expect(next.model).toBe("opus");
  });

  it("смена кранча не трогает поля настроек при той же ревизии", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    const next = form.sync({ rev: 3, crunchKey: "b", source: hub({ model: "sonnet", crunchEnabled: true }), current })!;
    expect(next.model).toBe("opus");
    expect(next.crunchEnabled).toBe(true);
  });

  it("поле, вернувшееся к значению хаба, снова следует за хабом", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    current.model = "haiku";
    current.model = "opus";
    expect(form.sync({ rev: 4, crunchKey: "a", source: hub({ model: "sonnet" }), current })!.model).toBe("sonnet");
  });

  it("после сохранения отправленные значения больше не считаются правкой", () => {
    const form = new SettingsFormSync();
    const current = form.sync({ rev: 3, crunchKey: "a", source: hub(), current: hub() })!;
    current.model = "haiku";
    form.saved(current);
    // Хаб принял и позже другое устройство сменило модель: лист показывает новое значение.
    expect(form.sync({ rev: 5, crunchKey: "a", source: hub({ model: "sonnet" }), current })!.model).toBe("sonnet");
  });
});
