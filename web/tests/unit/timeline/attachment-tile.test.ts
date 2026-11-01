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

  it("a_file_without_a_known_name_still_gets_a_readable_tile", () => {
    const { target } = render(ref({ name: "", mime: "", image: false, size: 0 }));
    expect(target.textContent).toContain("Вложение");
  });

  it("an_image_that_fails_to_load_falls_back_to_the_tile", () => {
    const { target } = render(ref({}));
    target.querySelector("img")!.dispatchEvent(new Event("error"));
    flushSync();
    expect(target.querySelector("img")).toBeNull();
    expect(target.textContent).toContain("shot.png");
  });
});
