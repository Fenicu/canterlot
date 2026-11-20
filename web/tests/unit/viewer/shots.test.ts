import { describe, expect, it } from "vitest";
import { declaredSize, downloadName, isVideoShot, opensViewer, shotCaption, shotFormat, shotPreviewSrc, shotSrc, videoPlayable } from "#lib/viewer/shots.js";

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

  it("preview_uses_the_rendition_and_full_uses_the_original", () => {
    expect(shotPreviewSrc({ media_id: "m" })).toBe("/api/v1/media/m/preview");
    expect(shotPreviewSrc({ media_id: "a", src: "/api/v1/attachments/a/content" })).toBe("/api/v1/attachments/a/preview");
  });

  it("a_shot_plays_as_video_by_its_kind_or_its_video_mime", () => {
    expect(isVideoShot({ media_id: "a", kind: "video" })).toBe(true);
    expect(isVideoShot({ media_id: "a", mime: "video/webm" })).toBe(true);
    expect(isVideoShot({ media_id: "a", mime: "image/png" })).toBe(false);
    expect(isVideoShot({ media_id: "a", kind: "image", mime: "video/mp4" })).toBe(false);
    expect(isVideoShot({ media_id: "a" })).toBe(false);
  });

  it("a_video_is_unplayable_when_the_browser_answers_no_for_its_type", () => {
    const answers = (known: Record<string, CanPlayTypeResult>) => (type: string): CanPlayTypeResult => known[type] ?? "";
    expect(videoPlayable("video/webm", answers({ "video/webm": "maybe" }))).toBe(true);
    expect(videoPlayable("video/mp4", answers({ "video/mp4": "probably" }))).toBe(true);
    expect(videoPlayable("video/x-unknown", answers({}))).toBe(false);
    expect(videoPlayable("video/webm; codecs=vp9", answers({}))).toBe(false);
    // Тип не назван: решает само воспроизведение, ошибку даст событие error.
    expect(videoPlayable(undefined, answers({}))).toBe(true);
    expect(videoPlayable("", answers({}))).toBe(true);
    // Chrome отвечает на QuickTime пустой строкой, но H.264 в .mov играет: пробуем играть.
    expect(videoPlayable("video/quicktime", answers({}))).toBe(true);
  });

  it("plain_click_opens_the_viewer_and_modified_clicks_stay_with_the_browser", () => {
    const click = { button: 0, ctrlKey: false, metaKey: false, shiftKey: false, altKey: false, defaultPrevented: false };
    expect(opensViewer(click)).toBe(true);
    // Новая вкладка, новое окно, сохранение и средняя кнопка остаются браузеру.
    expect(opensViewer({ ...click, ctrlKey: true })).toBe(false);
    expect(opensViewer({ ...click, metaKey: true })).toBe(false);
    expect(opensViewer({ ...click, shiftKey: true })).toBe(false);
    expect(opensViewer({ ...click, altKey: true })).toBe(false);
    expect(opensViewer({ ...click, button: 1 })).toBe(false);
    expect(opensViewer({ ...click, defaultPrevented: true })).toBe(false);
  });
});
