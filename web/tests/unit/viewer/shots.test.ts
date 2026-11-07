import { describe, expect, it } from "vitest";
import { declaredSize, downloadName, preloads, shotCaption, shotFormat, shotSrc } from "#lib/viewer/shots.js";

describe("viewer/shots", () => {
  it("media_and_attachments_have_their_own_addresses", () => {
    expect(shotSrc({ media_id: "m-1" })).toBe("/api/v1/media/m-1");
    expect(shotSrc({ media_id: "a-1", src: "/api/v1/attachments/a-1/content" })).toBe("/api/v1/attachments/a-1/content");
  });

  it("save_keeps_the_file_name_and_names_answer_media_by_format", () => {
    expect(downloadName({ media_id: "a-1", name: "screen.png" })).toBe("screen.png");
    expect(downloadName({ media_id: "0123456789", mime: "image/jpeg" })).toBe("картинка-01234567.jpg");
    expect(downloadName({ media_id: "0123456789", mime: "image/webp" })).toBe("картинка-01234567.webp");
    expect(downloadName({ media_id: "0123456789" })).toBe("картинка-01234567.png");
  });

  it("caption_prefers_its_own_text_and_falls_back_to_size_and_format", () => {
    expect(shotCaption({ media_id: "m", caption: "Отчёт · 2 КиБ" }, { width: 1, height: 1 })).toBe("Отчёт · 2 КиБ");
    expect(shotCaption({ media_id: "m", mime: "image/png" }, { width: 1440, height: 900 })).toBe("1440 × 900 · PNG");
    expect(shotCaption({ media_id: "m", mime: "image/svg+xml" }, null)).toBe("SVG");
    expect(shotFormat({ media_id: "m" })).toBe("");
  });

  it("size_is_known_only_when_both_sides_are_positive", () => {
    expect(declaredSize({ media_id: "m", width: 10, height: 20 })).toEqual({ width: 10, height: 20 });
    expect(declaredSize({ media_id: "m", width: null, height: 20 })).toBeNull();
    expect(declaredSize({ media_id: "m", width: 0, height: 0 })).toBeNull();
  });

  it("heavy_neighbours_are_not_preloaded", () => {
    expect(preloads({ media_id: "m" })).toBe(true);
    expect(preloads({ media_id: "m", size: 300_000 })).toBe(true);
    expect(preloads({ media_id: "m", size: 60 * 1024 * 1024 })).toBe(false);
  });
});
