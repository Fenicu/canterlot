import { describe, expect, it } from "vitest";
import { formatRejectedSettings } from "#lib/screens/chat/SessionSettings.svelte";

describe("chat/settings", () => {
  it("rejected_settings_event_is_shown_as_text", () => {
    expect(
      formatRejectedSettings({
        keys: ["model"],
        reason: "settings_unavailable",
      }),
    ).toBe("Настройка недоступна и сброшена: модель");

    expect(
      formatRejectedSettings({
        keys: ["model", "effort"],
        reason: "settings_unavailable",
      }),
    ).toBe("Настройка недоступна и сброшена: модель, усилие");

    expect(
      formatRejectedSettings({
        keys: ["permission_mode"],
        reason: "settings_unavailable",
      }),
    ).toBe("Настройка недоступна и сброшена: режим разрешений");
  });
});
