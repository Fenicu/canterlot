import { describe, expect, it } from "vitest";
import { icons } from "#lib/ui/icons.js";

describe("ui/icons", () => {
  it("paperclip_is_one_stroke_without_a_stray_line", () => {
    expect(icons.attach.match(/m/gi)).toHaveLength(1);
  });
});
