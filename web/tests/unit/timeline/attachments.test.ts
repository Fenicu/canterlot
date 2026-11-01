import { describe, expect, it } from "vitest";
import { THUMB_MAX_BYTES, attachmentKind, attachmentUrl, showsThumb } from "#lib/timeline/attachments.js";

const file = (name: string, mime: string, size = 1000, image = false) => ({ name, mime, size, image });

describe("timeline/attachments", () => {
  it("images_are_told_by_the_flag_the_mime_or_the_extension_of_an_untyped_file", () => {
    expect(attachmentKind(file("a.bin", "application/octet-stream", 1, true))).toBe("image");
    expect(attachmentKind(file("a", "image/webp"))).toBe("image");
    expect(attachmentKind(file("Screenshot.JPG", ""))).toBe("image");
    expect(attachmentKind(file("shot.png", "application/octet-stream"))).toBe("image");
    expect(attachmentKind(file("notes.txt", "text/plain"))).toBe("file");
    expect(attachmentKind(file("scan.png", "application/pdf"))).toBe("file");
  });

  it("video_is_told_by_the_mime_or_the_extension_of_an_untyped_file", () => {
    expect(attachmentKind(file("a", "video/mp4"))).toBe("video");
    expect(attachmentKind(file("clip.mov", ""))).toBe("video");
    expect(attachmentKind(file("clip.mov", "application/octet-stream"))).toBe("video");
    expect(attachmentKind(file("clip.mov", "text/plain"))).toBe("file");
  });

  it("a_huge_original_gets_a_tile_instead_of_a_decoded_thumbnail", () => {
    expect(showsThumb(file("a.png", "image/png", THUMB_MAX_BYTES))).toBe(true);
    expect(showsThumb(file("a.png", "image/png", THUMB_MAX_BYTES + 1))).toBe(false);
    expect(showsThumb(file("a.pdf", "application/pdf", 10))).toBe(false);
  });

  it("the_url_is_the_authorised_content_route_of_the_attachment", () => {
    expect(attachmentUrl("0190-ab")).toBe("/api/v1/attachments/0190-ab/content");
  });
});
