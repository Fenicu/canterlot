import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import AttachmentTile from "#lib/timeline/AttachmentTile.svelte";
import { THUMB_MAX_BYTES } from "#lib/timeline/attachments.js";
import type { AttachmentRef } from "#lib/stores/inputs.js";

const ref = (over: Partial<AttachmentRef>): AttachmentRef => ({
  attachment_id: "att-1",
  name: "shot.png",
  mime: "image/png",
  size: 2048,
  sha256: "",
  image: true,
  ...over,
});

describe("timeline/AttachmentTile", () => {
  let instance: ReturnType<typeof mount> | undefined;
  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function render(file: AttachmentRef, onopen = vi.fn()) {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(AttachmentTile, { target, props: { file, onopen } });
    flushSync();
    return { target, onopen };
  }

  it("a_small_image_is_a_lazy_thumbnail_from_the_authorised_route_that_opens_on_tap", () => {
    const { target, onopen } = render(ref({}));
    const img = target.querySelector("img")!;
    expect(img.getAttribute("src")).toBe("/api/v1/attachments/att-1/content");
    expect(img.getAttribute("loading")).toBe("lazy");
    expect(img.getAttribute("decoding")).toBe("async");
    target.querySelector("button")!.click();
    expect(onopen).toHaveBeenCalledOnce();
  });

  it("a_huge_original_is_not_decoded_in_the_bubble_but_still_opens_on_tap", () => {
    const { target, onopen } = render(ref({ size: THUMB_MAX_BYTES + 1, name: "big.png" }));
    expect(target.querySelector("img")).toBeNull();
    expect(target.textContent).toContain("big.png");
    expect(target.textContent).toContain("12");
    target.querySelector("button")!.click();
    expect(onopen).toHaveBeenCalledOnce();
  });

  it("video_gets_a_video_tile_with_name_and_size", () => {
    const { target } = render(ref({ name: "clip.mp4", mime: "video/mp4", image: false, size: 5 * 1024 * 1024 }));
    const tile = target.querySelector("a")!;
    expect(tile.getAttribute("data-kind")).toBe("video");
    expect(tile.getAttribute("href")).toBe("/api/v1/attachments/att-1/content");
    expect(tile.textContent).toContain("clip.mp4");
    expect(tile.textContent).toContain("5 МиБ");
    expect(tile.querySelector("svg")).not.toBeNull();
  });

  it("another_file_shows_name_and_size", () => {
    const { target } = render(ref({ name: "report.pdf", mime: "application/pdf", image: false, size: 1536 }));
    expect(target.querySelector("a")!.textContent).toContain("report.pdf");
    expect(target.textContent).toContain("1,5 КиБ");
    expect(target.querySelector("img")).toBeNull();
  });

  it("markdown_gets_a_preview_button_and_a_separate_download", () => {
    const { target } = render(ref({ name: "README.md", mime: "text/markdown", image: false, size: 2048 }));
    const open = target.querySelector('button[aria-label="Открыть документ README.md"]');
    expect(open).not.toBeNull();
    expect(open!.textContent).toContain("README.md");
    const save = target.querySelector('a[download]');
    expect(save!.getAttribute("href")).toBe("/api/v1/attachments/att-1/content");
    expect(save!.getAttribute("download")).toBe("README.md");
    expect(save!.getAttribute("aria-label")).toBe("Скачать README.md");
  });

  it("word_and_excel_files_name_their_kind_and_keep_the_extension_glyph_and_download", () => {
    const { target } = render(ref({ name: "Отчёт.docx", mime: "application/octet-stream", image: false, size: 4096 }));
    const open = target.querySelector('button[aria-label="Открыть документ Отчёт.docx"]')!;
    expect(open.getAttribute("data-kind")).toBe("word");
    expect(open.querySelector(".glyph")!.textContent).toBe("DOCX");
    expect(open.querySelector(".meta")!.textContent).toBe("Документ Word · 4 КиБ");
    expect(target.querySelector('a[download]')!.getAttribute("aria-label")).toBe("Скачать Отчёт.docx");
    void unmount(instance!);
    instance = undefined;
    document.body.textContent = "";

    const sheet = render(ref({ name: "table.xlsx", mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", image: false, size: 2048 }));
    const tile = sheet.target.querySelector('button[aria-label="Открыть таблицу table.xlsx"]')!;
    expect(tile.querySelector(".glyph")!.textContent).toBe("XLSX");
    expect(tile.querySelector(".meta")!.textContent).toBe("Таблица Excel · 2 КиБ");
  });

  it("a_file_without_a_known_name_still_gets_a_readable_tile", () => {
    const { target } = render(ref({ name: "", mime: "", image: false, size: 0 }));
    expect(target.textContent).toContain("Вложение");
  });

  it("an_empty_file_is_a_download_tile_with_zero_bytes_even_with_an_image_name", () => {
    for (const [name, mime] of [["empty.txt", "text/plain"], ["empty.png", "application/octet-stream"], ["clip.mp4", "application/octet-stream"]]) {
      const { target } = render(ref({ name, mime, image: false, size: 0 }));
      expect(target.querySelector("img")).toBeNull();
      const tile = target.querySelector("a")!;
      expect(tile.getAttribute("data-kind")).toBe("file");
      expect(tile.textContent).toContain(name);
      expect(tile.querySelector(".meta")!.textContent).toBe("0 Б");
      void unmount(instance!);
      instance = undefined;
      document.body.textContent = "";
    }
  });

  it("an_image_that_fails_to_load_falls_back_to_the_tile", () => {
    const { target } = render(ref({}));
    target.querySelector("img")!.dispatchEvent(new Event("error"));
    flushSync();
    expect(target.querySelector("img")).toBeNull();
    expect(target.textContent).toContain("shot.png");
  });
});
